//! Verifies the exit-code matrix promised in
//! [`docs/reference/exit-codes.md`](../../docs/reference/exit-codes.md).
//!
//! Each variant of `cgn_core::Error` maps to exactly one process exit
//! code; the documentation, the systemd `SuccessExitStatus` lists, and
//! the Kubernetes runbook all assume this mapping. A regression here
//! would silently break production restart logic, so this test guards
//! the mapping at every PR.

use cgn_core::{exit_code, Error};

#[test]
fn config_errors_exit_3() {
    assert_eq!(exit_code(&Error::Config("missing field".into())), 3);
}

#[test]
fn invalid_argument_exits_2() {
    assert_eq!(exit_code(&Error::InvalidArgument("bad flag".into())), 2);
}

#[test]
fn unavailable_dependencies_exit_4() {
    assert_eq!(exit_code(&Error::Etcd("connect".into())), 4);
    assert_eq!(exit_code(&Error::Unavailable("agent gone".into())), 4);
    assert_eq!(exit_code(&Error::NotFound("model".into())), 4);
}

#[test]
fn tls_errors_exit_5() {
    assert_eq!(exit_code(&Error::Tls("cert mismatch".into())), 5);
}

#[test]
fn port_in_use_exits_7() {
    let e = Error::Io(std::io::Error::from(std::io::ErrorKind::AddrInUse));
    assert_eq!(exit_code(&e), 7);
}

#[test]
fn other_io_exits_8() {
    let e = Error::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    assert_eq!(exit_code(&e), 8);
}

#[test]
fn internal_errors_exit_1() {
    assert_eq!(exit_code(&Error::Internal("oops".into())), 1);
}
