use dt::parser::Parser;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_parse_rust_function_line_numbers() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("test.rs");
    fs::write(&file_path, "\n\nfn hello() {\n    println!(\"hi\");\n}\n").unwrap();

    let mut p = Parser::new().unwrap();
    let result = p.parse_file(
        &file_path.to_string_lossy(),
        "testproj",
        &dir.path().to_string_lossy(),
    ).unwrap();

    assert_eq!(result.methods.len(), 1);
    let m = &result.methods[0];
    assert_eq!(m.start_line, 3, "start_line should be 3 (0-indexed row 2 + 1)");
    assert_eq!(m.end_line, 5, "end_line should be 5");
    assert_eq!(m.name, "hello");
}

#[test]
fn test_unsupported_language_returns_empty() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("test.c");
    fs::write(&file_path, "int main() { return 0; }").unwrap();

    let mut p = Parser::new().unwrap();
    let result = p.parse_file(
        &file_path.to_string_lossy(),
        "testproj",
        &dir.path().to_string_lossy(),
    ).unwrap();

    assert!(result.methods.is_empty(), "unsupported language should return empty");
    assert!(result.classes.is_empty());
}

#[test]
fn test_class_name_is_populated() {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("test.py");
    fs::write(&file_path, "class MyClass:\n    def my_method(self):\n        pass\n").unwrap();

    let mut p = Parser::new().unwrap();
    let result = p.parse_file(
        &file_path.to_string_lossy(),
        "testproj",
        &dir.path().to_string_lossy(),
    ).unwrap();

    let found = result.methods.iter().any(|m| m.name == "my_method" && m.class_name == "MyClass");
    assert!(found, "class_name should be 'MyClass' for method 'my_method'");
}

#[test]
fn test_tempdir_cleaned_up() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();
    let file_path = path.join("test.rs");
    fs::write(&file_path, "fn foo() {}").unwrap();

    let mut p = Parser::new().unwrap();
    let _ = p.parse_file(&file_path.to_string_lossy(), "test", &path.to_string_lossy());

    drop(dir);
    assert!(!path.exists(), "temp dir should be cleaned up after drop");
}
