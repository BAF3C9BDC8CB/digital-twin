//! Entity ID generation rules for the Digital Twin system.
//!
//! Implements the `dt://entity/...` URI scheme used to uniquely identify
//! code entities across projects.

/// Generate a method ID: `dt://entity/{project}/class/{class_name}/method/{method_name}@{line}`
pub fn make_method_id(project: &str, file_path: &str, class_name: &str, method_name: &str, start_line: usize) -> String {
    // Sanitize slashes in file_path
    let _clean_file = file_path.replace('/', ".");
    format!(
        "dt://entity/{project}/class/{class_name}/method/{method_name}@{start_line}",
        project = project,
        class_name = class_name,
        method_name = method_name,
        start_line = start_line,
    )
}

/// Generate a class ID: `dt://entity/{project}/package/{package}/class/{name}`
pub fn make_class_id(project: &str, package: &str, class_name: &str) -> String {
    let pkg = if package.is_empty() { "_root" } else { package };
    format!(
        "dt://entity/{project}/package/{package}/class/{name}",
        project = project,
        package = pkg,
        name = class_name,
    )
}

/// Generate a module ID: `dt://entity/{project}/module/{name}`
pub fn make_module_id(project: &str, module_name: &str) -> String {
    format!(
        "dt://entity/{project}/module/{name}",
        project = project,
        name = module_name,
    )
}

/// Generate a placeholder entity ID.
pub fn placeholder_id() -> String {
    "dt://entity/placeholder".to_string()
}

/// Generate a document ID: `dt://doc/{project}/{file_path}`
pub fn make_document_id(project: &str, file_path: &str) -> String {
    let clean_path = file_path.replace('\\', "/");
    format!(
        "dt://doc/{project}/{path}",
        project = project,
        path = clean_path,
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_id_structure() {
        let id = make_method_id("mytest", "src/foo.rs", "Foo", "bar", 42);
        assert!(id.starts_with("dt://entity/mytest/"));
        assert!(id.contains("/class/Foo/"));
        assert!(id.contains("/method/bar@42"));
    }

    #[test]
    fn class_id_structure() {
        let id = make_class_id("mytest", "com.example", "MyClass");
        assert!(id.starts_with("dt://entity/mytest/"));
        assert!(id.contains("/package/com.example/"));
        assert!(id.contains("/class/MyClass"));
    }

    #[test]
    fn class_id_root_package() {
        let id = make_class_id("mytest", "", "Standalone");
        assert!(id.contains("/package/_root/"));
    }

    #[test]
    fn module_id_structure() {
        let id = make_module_id("mytest", "pay-service");
        assert!(id.starts_with("dt://entity/mytest/"));
        assert!(id.contains("/module/pay-service"));
    }

    #[test]
    fn placeholder_id_returns_string() {
        assert_eq!(placeholder_id(), "dt://entity/placeholder");
    }
}
