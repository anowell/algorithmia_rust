//! File module for managing Algorithmia Data Files
//!
//! # Examples
//!
//! ```no_run
//! use algorithmia::Algorithmia;
//! # fn main() -> Result<(), Box<std::error::Error>> {
//! let client = Algorithmia::client("111112222233333444445555566")?;
//! let my_file = client.file(".my/my_dir/some_filename");
//!
//! my_file.put("file_contents")?;
//! # Ok(())
//! # }
//! ```

use super::{parse_data_uri, parse_headers};
use crate::client::HttpClient;
use crate::data::{DataType, HasDataPath};
use crate::error::{ApiError, Error, ErrorKind, ResultExt};
use crate::Body;
use chrono::{DateTime, TimeZone, Utc};
use reqwest::StatusCode;
use std::io::{self, Read};

/// Response and reader when downloading a `DataFile`
pub struct FileData {
    /// Size of file in bytes
    pub size: u64,
    /// Last modified timestamp
    pub last_modified: DateTime<Utc>,
    data: Box<Read>,
}

impl Read for FileData {
    #[inline]
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.data.read(buf)
    }
}

impl FileData {
    /// Reads the result into a byte vector
    ///
    /// This is a convenience wrapper around `Read::read_to_end`
    /// that allocates once with capacity of `self.size`.
    pub fn into_bytes(mut self) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(self.size as usize);
        self.read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    /// Reads the result into a `String`
    ///
    /// This is a convenience wrapper around `Read::read_to_string`
    /// that allocates once with capacity of `self.size`.
    pub fn into_string(mut self) -> io::Result<String> {
        let mut text = String::with_capacity(self.size as usize);
        self.read_to_string(&mut text)?;
        Ok(text)
    }
}

/// Algorithmia data file
pub struct DataFile {
    path: String,
    client: HttpClient,
}

impl HasDataPath for DataFile {
    #[doc(hidden)]
    fn new(client: HttpClient, path: &str) -> Self {
        DataFile {
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

impl DataFile {
    /// Write to the Algorithmia Data API
    ///
    /// # Examples
    /// ```no_run
    /// # use algorithmia::Algorithmia;
    /// # use std::fs::File;
    /// # fn main() -> Result<(), Box<std::error::Error>> {
    /// let client = Algorithmia::client("111112222233333444445555566")?;
    ///
    /// client.file(".my/my_dir/string.txt").put("file_contents")?;
    /// client.file(".my/my_dir/bytes.txt").put("file_contents".as_bytes())?;
    ///
    /// let file = File::open("/path/to/file.jpg")?;
    /// client.file(".my/my_dir/file.jpg").put(file)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn put<B>(&self, body: B) -> Result<(), Error>
    where
        B: Into<Body>,
    {
        let url = self.to_url()?;
        let mut res = self
            .client
            .put(url)
            .body(body)
            .send()
            .chain_err(|| ErrorKind::Http(format!("writing file '{}'", self.to_data_uri())))?;
        let mut res_json = String::new();
        res.read_to_string(&mut res_json)
            .chain_err(|| ErrorKind::Io(format!("writing file '{}'", self.to_data_uri())))?;

        match res.status() {
            status if status.is_success() => Ok(()),
            StatusCode::NOT_FOUND => Err(ErrorKind::NotFound(self.to_url().unwrap()).into()),
            status => Err(ApiError::from_json_or_status(&res_json, status).into()),
        }
    }

    /// Get a file from the Algorithmia Data API
    ///
    /// # Examples
    /// ```no_run
    /// # use algorithmia::Algorithmia;
    /// # use std::io::Read;
    /// # fn main() -> Result<(), Box<std::error::Error>> {
    /// let client = Algorithmia::client("111112222233333444445555566")?;
    /// let my_file = client.file(".my/my_dir/sample.txt");
    ///
    /// let data = my_file.get()?.into_string()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn get(&self) -> Result<FileData, Error> {
        let url = self.to_url()?;
        let req = self.client.get(url);
        let res = req
            .send()
            .chain_err(|| ErrorKind::Http(format!("downloading file '{}'", self.to_data_uri())))?;

        match res.status() {
            StatusCode::OK => {
                let metadata = parse_headers(res.headers())?;
                match metadata.data_type {
                    DataType::File => (),
                    DataType::Dir => {
                        return Err(
                            ErrorKind::UnexpectedDataType("file", "directory".to_string()).into(),
                        );
                    }
                }

                Ok(FileData {
                    size: metadata.content_length.unwrap_or(0),
                    last_modified: metadata
                        .last_modified
                        .unwrap_or_else(|| Utc.ymd(2015, 3, 14).and_hms(8, 0, 0)),
                    data: Box::new(res),
                })
            }
            StatusCode::NOT_FOUND => Err(Error::from(ErrorKind::NotFound(self.to_url().unwrap()))),
            status => Err(ApiError::from(status.to_string()).into()),
        }
    }

    /// Delete a file from from the Algorithmia Data API
    ///
    /// # Examples
    /// ```no_run
    /// # use algorithmia::Algorithmia;
    /// # fn main() -> Result<(), Box<std::error::Error>> {
    /// let client = Algorithmia::client("111112222233333444445555566")?;
    /// let my_file = client.file(".my/my_dir/sample.txt");
    ///
    /// match my_file.delete() {
    ///   Ok(_) => println!("Successfully deleted file"),
    ///   Err(err) => println!("Error deleting file: {}", err),
    /// };
    /// # Ok(())
    /// # }
    /// ```
    pub fn delete(&self) -> Result<(), Error> {
        let url = self.to_url()?;
        let req = self.client.delete(url);
        let mut res = req
            .send()
            .chain_err(|| ErrorKind::Http(format!("deleting file '{}'", self.to_data_uri())))?;
        let mut res_json = String::new();
        res.read_to_string(&mut res_json)
            .chain_err(|| ErrorKind::Io(format!("deleting file '{}'", self.to_data_uri())))?;

        match res.status() {
            status if status.is_success() => Ok(()),
            StatusCode::NOT_FOUND => Err(ErrorKind::NotFound(self.to_url().unwrap()).into()),
            status => Err(ApiError::from_json_or_status(&res_json, status).into()),
        }
    }
}
