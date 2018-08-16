//! Directory module for managing Algorithmia Data Directories
//!
//! # Examples
//!
//! ```no_run
//! use algorithmia::Algorithmia;
//! use algorithmia::data::DataAcl;
//!
//! let client = Algorithmia::client("111112222233333444445555566");
//! let my_dir = client.dir(".my/my_dir");
//!
//! my_dir.create(DataAcl::default()).unwrap();
//! my_dir.put_file("/path/to/file").unwrap();
//! ```

use client::HttpClient;
use error::{ApiError, ErrorKind, Result, ResultExt};
use data::{DataItem, DataDirItem, DataFileItem, HasDataPath, DataFile};
use super::parse_data_uri;
use super::header::XDataType;
use serde_json;

use std::io::Read;
use std::fs::File;
use std::path::Path;
use std::vec::IntoIter;

use chrono::{DateTime, Utc};
use mime;
use reqwest::header::ContentType;
use reqwest::StatusCode;

/// Algorithmia Data Directory
pub struct DataDir {
    path: String,
    client: HttpClient,
}

#[derive(Debug, Deserialize)]
struct DeletedResponse {
    result: DirectoryDeleted,
}

/// Response when deleting a file form the Data API
#[derive(Debug, Deserialize)]
pub struct DirectoryDeleted {
    /// Number of files that were deleted
    ///
    /// Note: some backing stores may indicate deletion succeeds for non-existing files
    pub deleted: u64,
    // Placeholder for API stability if additional fields are added later
    #[serde(skip_deserializing)]
    _dummy: (),
}

#[derive(Debug, Deserialize, Serialize)]
struct FolderItem {
    pub name: String,
    pub acl: Option<DataAcl>,
}

#[derive(Debug, Deserialize)]
struct FileItem {
    pub filename: String,
    pub size: u64,
    pub last_modified: DateTime<Utc>,
}

/// ACL that indicates permissions for a `DataDir`
/// See also: [`ReadAcl`](enum.ReadAcl.html) enum to construct a `DataACL`
#[derive(Debug, Deserialize, Serialize)]
pub struct DataAcl {
    /// Read ACL
    pub read: Vec<String>,
    // Placeholder for stability with API additions
    _dummy: (),
}

/// Read access control values
pub enum ReadAcl {
    /// Readable only by owner
    Private,
    /// Readable by owner's algorithms (regardless of caller)
    MyAlgorithms,
    /// Readable by any user
    Public,

    /// Non-exhaustive for API stability if ACL types are added
    #[doc(hidden)]
    __Nonexhaustive,
}

impl Default for DataAcl {
    fn default() -> Self {
        ReadAcl::MyAlgorithms.into()
    }
}

impl From<ReadAcl> for DataAcl {
    fn from(acl: ReadAcl) -> Self {
        match acl {
            ReadAcl::Private |
            ReadAcl::__Nonexhaustive => {
                DataAcl {
                    read: vec![],
                    _dummy: (),
                }
            }
            ReadAcl::MyAlgorithms => {
                DataAcl {
                    read: vec!["algo://.my/*".into()],
                    _dummy: (),
                }
            }
            ReadAcl::Public => {
                DataAcl {
                    read: vec!["user://*".into()],
                    _dummy: (),
                }
            }
        }
    }
}

/// Response when querying an existing Directory
#[derive(Debug, Deserialize)]
struct DirectoryShow {
    pub acl: Option<DataAcl>,
    pub folders: Option<Vec<FolderItem>>,
    pub files: Option<Vec<FileItem>>,
    pub marker: Option<String>,
}

/// Iterator over the listing of a `DataDir`
pub struct DirectoryListing<'a> {
    /// ACL indicates permissions for this `DataDir`
    pub acl: Option<DataAcl>,
    dir: &'a DataDir,
    folders: IntoIter<FolderItem>,
    files: IntoIter<FileItem>,
    marker: Option<String>,
    query_count: u32,
}

impl<'a> DirectoryListing<'a> {
    fn new(dir: &'a DataDir) -> DirectoryListing<'a> {
        DirectoryListing {
            acl: None,
            dir: dir,
            folders: Vec::new().into_iter(),
            files: Vec::new().into_iter(),
            marker: None,
            query_count: 0,
        }
    }
}

impl<'a> Iterator for DirectoryListing<'a> {
    type Item = Result<DataItem>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.folders.next() {
            // Return folders first
            Some(d) => Some(Ok(
                DataItem::Dir(DataDirItem { dir: self.dir.child(&d.name) }),
            )),
            None => {
                match self.files.next() {
                    // Return files second
                    Some(f) => {
                        Some(Ok(DataItem::File(DataFileItem {
                            size: f.size,
                            last_modified: f.last_modified,
                            file: self.dir.child(&f.filename),
                        })))
                    }
                    None => {
                        // Query if there is another page of files/folders
                        if self.query_count == 0 || self.marker.is_some() {
                            self.query_count += 1;
                            match get_directory(self.dir, self.marker.clone()) {
                                Ok(ds) => {
                                    self.folders = ds.folders.unwrap_or_else(Vec::new).into_iter();
                                    self.files = ds.files.unwrap_or_else(Vec::new).into_iter();
                                    self.marker = ds.marker;
                                    self.next()
                                }
                                Err(err) => Some(Err(err)),
                            }
                        } else {
                            None
                        }
                    }
                }
            }
        }
    }
}

fn get_directory(dir: &DataDir, marker: Option<String>) -> Result<DirectoryShow> {
    let mut url = dir.to_url()?;
    if let Some(ref m) = marker {
        url.query_pairs_mut().append_pair("marker", m);
    }

    let mut res = dir.client.get(url).send().chain_err(|| {
        ErrorKind::Http(format!("listing directory '{}'", dir.to_data_uri()))
    })?;

    if res.status().is_success() {
        if let Some(data_type) = res.headers().get::<XDataType>() {
            if "directory" != data_type.as_str() {
                return Err(
                    ErrorKind::UnexpectedDataType("directory", data_type.to_string()).into(),
                );
            }
        }
    }

    let mut res_json = String::new();
    res.read_to_string(&mut res_json)
        .chain_err(|| {
            ErrorKind::Io(format!("listing directory '{}'", dir.to_data_uri()))
        })?;

    match res.status() {
        status if status.is_success() => {
            serde_json::from_str(&res_json).chain_err(|| ErrorKind::DecodeJson("directory listing"))
        }
        StatusCode::NotFound => Err(ErrorKind::NotFound(dir.to_url().unwrap()).into()),
        status => Err(ApiError::from_json_or_status(&res_json, status).into())
    }
}

impl HasDataPath for DataDir {
    #[doc(hidden)]
    fn new(client: HttpClient, path: &str) -> Self {
        DataDir {
            client: client,
            path: parse_data_uri(path).to_string(),
        }
    }
    #[doc(hidden)]
    fn path(&self) -> &str {
        &self.path
    }
    #[doc(hidden)]
    fn client(&self) -> &HttpClient {
        &self.client
    }
}

impl DataDir {
    /// Display Directory details if it exists
    ///
    /// # Examples
    /// ```no_run
    /// # use algorithmia::Algorithmia;
    /// # use algorithmia::data::{DataItem, HasDataPath};
    /// let client = Algorithmia::client("111112222233333444445555566");
    /// let my_dir = client.dir(".my/my_dir");
    /// let dir_list = my_dir.list();
    /// for entry in dir_list {
    ///     match entry {
    ///         Ok(DataItem::File(f)) => println!("File: {}", f.to_data_uri()),
    ///         Ok(DataItem::Dir(d)) => println!("Dir: {}", d.to_data_uri()),
    ///         Err(err) => { println!("Error: {}", err); break; },
    ///     }
    /// };
    /// ```
    pub fn list(&self) -> DirectoryListing {
        DirectoryListing::new(self)
    }

    /// Create a Directory
    ///
    /// Use `DataAcl::default()` or the `ReadAcl` enum to set the ACL
    ///
    /// # Examples
    /// ```no_run
    /// # use algorithmia::Algorithmia;
    /// # use algorithmia::data::DataAcl;
    /// let client = Algorithmia::client("111112222233333444445555566");
    /// let my_dir = client.dir(".my/my_dir");
    /// match my_dir.create(DataAcl::default()) {
    ///   Ok(_) => println!("Successfully created Directory"),
    ///   Err(e) => println!("Error created directory: {}", e),
    /// };
    /// ```
    pub fn create<Acl: Into<DataAcl>>(&self, acl: Acl) -> Result<()> {
        let parent = self.parent()
            .ok_or_else(|| ErrorKind::InvalidDataUri(self.to_data_uri()))?;
        let parent_url = parent.to_url()?;

        let input_data = FolderItem {
            name: self.basename()
                .ok_or_else(|| ErrorKind::InvalidDataUri(self.to_data_uri()))?
                .into(),
            acl: Some(acl.into()),
        };
        let raw_input = serde_json::to_vec(&input_data)
            .chain_err(|| ErrorKind::EncodeJson("directory creation parameters"))?;

        // POST request
        let mut res = self.client
            .post(parent_url)
            .header(ContentType(mime::APPLICATION_JSON))
            .body(raw_input)
            .send()
            .chain_err(|| {
                ErrorKind::Http(format!("creating directory '{}'", self.to_data_uri()))
            })?;


        match res.status() {
            status if status.is_success() => Ok(()),
            StatusCode::NotFound => Err(ErrorKind::NotFound(self.to_url().unwrap()).into()),
            status => {
                let mut res_json = String::new();
                res.read_to_string(&mut res_json)
                    .chain_err(|| {
                        ErrorKind::Io(format!("creating directory '{}'", self.to_data_uri()))
                    })?;
                Err(ApiError::from_json_or_status(&res_json, status).into())
            }
        }

    }


    /// Delete a Directory
    ///
    /// # Examples
    /// ```no_run
    /// # use algorithmia::Algorithmia;
    /// let client = Algorithmia::client("111112222233333444445555566");
    /// let my_dir = client.dir(".my/my_dir");
    /// match my_dir.delete(false) {
    ///   Ok(_) => println!("Successfully deleted Directory"),
    ///   Err(err) => println!("Error deleting directory: {}", err),
    /// };
    /// ```
    pub fn delete(&self, force: bool) -> Result<DirectoryDeleted> {
        // DELETE request
        let mut url = self.to_url()?;
        if force {
            url.query_pairs_mut().append_pair("force", "true");
        }

        // Parse response
        let mut res = self.client.delete(url).send()
            .chain_err(|| {
                ErrorKind::Http(format!("deleting directory '{}'", self.to_data_uri()))
            })?;
        let mut res_json = String::new();
        res.read_to_string(&mut res_json)
            .chain_err(|| {
                ErrorKind::Io(format!("deleting directory '{}'", self.to_data_uri()))
            })?;


        match res.status() {
            status if status.is_success() => {
                serde_json::from_str::<DeletedResponse>(&res_json)
                    .map(|res| res.result)
                    .chain_err(|| ErrorKind::DecodeJson("directory deletion response"))
            }
            StatusCode::NotFound => Err(ErrorKind::NotFound(self.to_url().unwrap()).into()),
            status => Err(ApiError::from_json_or_status(&res_json, status).into()),
        }
    }


    /// Upload a file to an existing Directory
    ///
    /// # Examples
    /// ```no_run
    /// # use algorithmia::prelude::*;
    /// let client = Algorithmia::client("111112222233333444445555566");
    /// let my_dir = client.dir(".my/my_dir");
    ///
    /// match my_dir.put_file("/path/to/file") {
    ///   Ok(_) => println!("Successfully uploaded to: {}", my_dir.to_data_uri()),
    ///   Err(err) => println!("Error uploading file: {}", err),
    /// };
    /// ```
    pub fn put_file<P: AsRef<Path>>(&self, file_path: P) -> Result<()> {
        let path_ref = file_path.as_ref();
        let file = File::open(path_ref)
            .chain_err(|| {
                ErrorKind::Io(format!("opening file for upload '{}'", path_ref.display()))
            })?;

        // Safe to unwrap: we've already opened the file or returned an error
        let filename = path_ref.file_name().unwrap().to_string_lossy();
        let data_file: DataFile = self.child(&filename);
        data_file.put(file)
    }

    /// Instantiate `DataFile` or `DataDir` as a child of this `DataDir`
    pub fn child<T: HasDataPath>(&self, filename: &str) -> T {
        let new_uri = match self.to_data_uri() {
            ref uri if uri.ends_with('/') => format!("{}{}", uri, filename),
            uri => format!("{}/{}", uri, filename),
        };
        T::new(self.client.clone(), &new_uri)
    }
}


#[cfg(test)]
mod tests {
    use data::HasDataPath;
    use super::*;
    use Algorithmia;

    fn mock_client() -> Algorithmia {
        Algorithmia::client("")
    }

    #[test]
    fn test_to_url() {
        let dir = mock_client().dir("data://anowell/foo");
        assert_eq!(
            dir.to_url().unwrap().path(),
            "/v1/connector/data/anowell/foo"
        );
    }

    #[test]
    fn test_to_data_uri() {
        let dir = mock_client().dir("/anowell/foo");
        assert_eq!(dir.to_data_uri(), "data://anowell/foo".to_string());
    }

    #[test]
    fn test_parent() {
        let dir = mock_client().dir("data://anowell/foo");
        let expected = mock_client().dir("data://anowell");
        assert_eq!(dir.parent().unwrap().path, expected.path);

        let dir = mock_client().dir("dropbox://anowell/foo");
        let expected = mock_client().dir("dropbox://anowell");
        assert_eq!(dir.parent().unwrap().path, expected.path);

        let dir = mock_client().dir("data://anowell");
        let expected = mock_client().dir("data://");
        assert_eq!(dir.parent().unwrap().path, expected.path);

        let dir = mock_client().dir("data://");
        assert!(dir.parent().is_none());
    }

    #[test]
    fn test_default_acl() {
        let acl: DataAcl = DataAcl::default();
        assert_eq!(acl.read, vec!["algo://.my/*".to_string()]);
    }

    #[test]
    fn test_private_acl() {
        let acl: DataAcl = ReadAcl::Private.into();
        assert!(acl.read.is_empty());
    }

    #[test]
    fn test_public_acl() {
        let acl: DataAcl = ReadAcl::Public.into();
        assert_eq!(acl.read, vec!["user://*".to_string()]);
    }

    #[test]
    fn test_myalgos_acl() {
        let acl: DataAcl = ReadAcl::MyAlgorithms.into();
        assert_eq!(acl.read, vec!["algo://.my/*".to_string()]);
    }

}