#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("IO Error: {0}")]
    IoError(std::io::ErrorKind),
}

impl From<std::io::ErrorKind> for WriteError {
    #[inline]
    fn from(value: std::io::ErrorKind) -> Self {
        Self::IoError(value)
    }
}

impl From<std::io::Error> for WriteError {
    #[inline]
    fn from(value: std::io::Error) -> Self {
        Self::IoError(value.kind())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("IO Error: {0}")]
    IoError(std::io::ErrorKind),
    #[error(
        "Allowed depth exceeded (billion laughs detected?), we are {depth} levels deep but only {limit} is allowed"
    )]
    AllowedDepthOverflow { depth: usize, limit: usize },
}

impl From<std::io::ErrorKind> for ReadError {
    #[inline]
    fn from(value: std::io::ErrorKind) -> Self {
        Self::IoError(value)
    }
}

impl From<std::io::Error> for ReadError {
    #[inline]
    fn from(value: std::io::Error) -> Self {
        Self::IoError(value.kind())
    }
}

pub type ReadResult<T> = Result<T, ReadError>;
pub type WriteResult<T> = Result<T, WriteError>;
