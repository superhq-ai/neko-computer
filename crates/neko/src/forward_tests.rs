use super::forward::{parse, Mapping};

#[test]
fn a_single_port_maps_to_itself() {
    assert_eq!(
        parse("8000").unwrap(),
        Mapping {
            host: 8000,
            guest: 8000
        }
    );
}

#[test]
fn host_and_guest_follow_docker_order() {
    assert_eq!(
        parse("3000:8000").unwrap(),
        Mapping {
            host: 3000,
            guest: 8000
        }
    );
}

#[test]
fn whitespace_is_forgiven() {
    assert_eq!(
        parse(" 3000 : 8000 ").unwrap(),
        Mapping {
            host: 3000,
            guest: 8000
        }
    );
}

#[test]
fn nonsense_is_refused_before_a_boot_is_spent() {
    for spec in [
        "", "http", "80:", ":80", "0", "8000:0", "-1", "70000", "80:80:80",
    ] {
        assert!(parse(spec).is_err(), "expected {spec:?} to be refused");
    }
}
