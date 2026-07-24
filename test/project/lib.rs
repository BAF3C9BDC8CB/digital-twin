/// Core library module.
pub mod types {
    /// A generic result wrapper.
    pub struct Result<T> {
        pub data: T,
        pub error: Option<String>,
    }

    impl<T> Result<T> {
        /// Create a new success result.
        pub fn ok(data: T) -> Self {
            Result { data, error: None }
        }
    }
}

/// Create a standard greeting message.
pub fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}
