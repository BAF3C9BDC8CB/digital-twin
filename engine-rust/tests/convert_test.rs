use dt::models::MethodBlock;
use dt::client::neo4j::MethodNode;
use dt::index::convert::build_payload;

#[test]
fn test_methodblock_to_methodnode() {
    let mb = MethodBlock {
        method_id: "abc123".into(),
        project: "test".into(),
        file_path: "src/main.rs".into(),
        language: "rust".into(),
        package_or_module: "".into(),
        class_name: "".into(),
        name: "main".into(),
        signature: "fn main()".into(),
        params: vec![],
        return_type: "".into(),
        source_code: "fn main() {}".into(),
        search_text: "search".into(),
        summary: "".into(),
        start_line: 1,
        end_line: 1,
        comment: "".into(),
        calls: vec!["println".into()],
    };

    let node: MethodNode = (&mb).into();
    assert_eq!(node.method_id, "abc123");
    assert_eq!(node.name, "main");
    assert_eq!(node.project, "test");
    assert_eq!(node.start_line, 1);
    assert_eq!(node.calls, vec!["println"]);
    assert_eq!(node.params, "");
}

#[test]
fn test_build_payload_has_all_keys() {
    let mb = MethodBlock {
        method_id: "abc123".into(),
        project: "test".into(),
        file_path: "src/main.rs".into(),
        language: "rust".into(),
        package_or_module: "".into(),
        class_name: "".into(),
        name: "main".into(),
        signature: "fn main()".into(),
        params: vec![],
        return_type: "".into(),
        source_code: "fn main() {}".into(),
        search_text: "search text".into(),
        summary: "".into(),
        start_line: 1,
        end_line: 3,
        comment: "// comment".into(),
        calls: vec!["println".into()],
    };

    let payload = build_payload(&mb);
    assert_eq!(payload.get("method_id").unwrap().as_str().unwrap(), "abc123");
    assert_eq!(payload.get("project").unwrap().as_str().unwrap(), "test");
    assert_eq!(payload.get("file_path").unwrap().as_str().unwrap(), "src/main.rs");
    assert_eq!(payload.get("start_line").unwrap().as_u64().unwrap(), 1);
    assert!(payload.get("search_text").unwrap().as_str().unwrap().contains("search"));
}
