use super::mount::parse;

#[test]
fn a_mount_is_read_only_unless_it_asks_otherwise() {
    assert!(parse(".:/workspace").unwrap().read_only);
    assert!(parse(".:/workspace:ro").unwrap().read_only);
    assert!(!parse(".:/workspace:rw").unwrap().read_only);
}

#[test]
fn the_host_path_is_resolved_before_a_boot_is_spent() {
    let mount = parse(".:/src").unwrap();
    assert!(mount.host_path.starts_with('/'), "{}", mount.host_path);
    assert_eq!(mount.guest_path, "/src");
}

#[test]
fn a_trailing_slash_on_the_guest_path_is_forgiven() {
    assert_eq!(parse(".:/src/").unwrap().guest_path, "/src");
}

#[test]
fn whitespace_is_forgiven() {
    let mount = parse(" . : /src :rw").unwrap();
    assert_eq!(mount.guest_path, "/src");
    assert!(!mount.read_only);
}

#[test]
fn a_host_path_ending_in_a_mode_word_is_still_a_path() {
    // The mode is only the last field, so ":/rw" stays a guest path.
    assert_eq!(parse(".:/rw").unwrap().guest_path, "/rw");
}

#[test]
fn nonsense_is_refused_before_a_boot_is_spent() {
    for spec in [
        "",
        ".",
        "/workspace",
        ":/workspace",
        ".:workspace",
        ".:/",
        ".:/src:rwx",
        "./no-such-directory-here:/src",
    ] {
        assert!(parse(spec).is_err(), "expected {spec:?} to be refused");
    }
}
