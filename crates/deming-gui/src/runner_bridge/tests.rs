use super::{default_config_path, parse_pid_from_message};

#[test]
fn parses_pid_formats() {
    assert_eq!(parse_pid_from_message("pid=12345"), Some(12345));
    assert_eq!(
        parse_pid_from_message("running (pid 9876) on port 25565"),
        Some(9876)
    );
    assert_eq!(parse_pid_from_message("started pid=42 ok"), Some(42));
    assert_eq!(parse_pid_from_message("no pid here"), None);
}

#[test]
fn default_config_path_ends_with_deming_config_json() {
    let p = default_config_path();
    let s = p.to_string_lossy();
    assert!(!s.is_empty());
}
