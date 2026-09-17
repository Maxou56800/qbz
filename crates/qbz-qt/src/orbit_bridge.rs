//! Launch-gated Orbit laboratory. No operation is admitted without --orbit.
use cxx_qt::{CxxQtThread, Threading as _};
use cxx_qt_lib::QString;
use std::pin::Pin;
use std::sync::OnceLock;

#[cxx_qt::bridge]
pub mod qbz_orbit_bridge {
    extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }
    #[auto_cxx_name]
    extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qml_singleton]
        #[qproperty(bool, enabled)]
        #[qproperty(QString, state_json)]
        // Reserved for the integral context switch, NOT set by a host probe.
        // The shell banner already consumes these; verifying a host leaves
        // the current local context and all of its controls explicitly local.
        #[qproperty(bool, controlling_remote)]
        #[qproperty(bool, connection_lost)]
        #[qproperty(QString, host_name)]
        type QbzOrbit = super::QbzOrbitRust;
        #[qinvokable]
        fn boot(self: Pin<&mut QbzOrbit>);
        #[qinvokable]
        fn start_host(self: Pin<&mut QbzOrbit>, address: QString);
        #[qinvokable]
        fn stop_host(self: Pin<&mut QbzOrbit>);
        #[qinvokable]
        fn copy_access_key(self: Pin<&mut QbzOrbit>);
        #[qinvokable]
        fn verify_host(self: Pin<&mut QbzOrbit>, url: QString, token: QString);
        #[qinvokable]
        fn search_library(self: Pin<&mut QbzOrbit>, query: QString);
    }
    impl cxx_qt::Threading for QbzOrbit {}
}
use qbz_orbit_bridge::QbzOrbit;

pub struct QbzOrbitRust {
    enabled: bool,
    state_json: QString,
    controlling_remote: bool,
    connection_lost: bool,
    host_name: QString,
}
impl Default for QbzOrbitRust {
    fn default() -> Self {
        Self {
            enabled: crate::orbit_qt::enabled(),
            state_json: QString::from("{}"),
            controlling_remote: false,
            connection_lost: false,
            host_name: QString::default(),
        }
    }
}
static THREAD: OnceLock<CxxQtThread<QbzOrbit>> = OnceLock::new();
pub fn ui(f: impl FnOnce(Pin<&mut QbzOrbit>) + Send + 'static) {
    if let Some(thread) = THREAD.get() {
        let _ = thread.queue(f);
    }
}
impl QbzOrbit {
    pub fn boot(self: Pin<&mut Self>) {
        let _ = THREAD.set(self.qt_thread());
        if crate::orbit_qt::enabled() {
            crate::orbit_qt::publish();
        }
    }
    pub fn start_host(self: Pin<&mut Self>, address: QString) {
        crate::orbit_qt::start_host(address.to_string());
    }
    pub fn stop_host(self: Pin<&mut Self>) {
        crate::orbit_qt::stop_host();
    }
    pub fn copy_access_key(self: Pin<&mut Self>) {
        crate::orbit_qt::copy_access_key();
    }
    pub fn verify_host(self: Pin<&mut Self>, url: QString, token: QString) {
        crate::orbit_qt::verify_host(url.to_string(), token.to_string());
    }
    pub fn search_library(self: Pin<&mut Self>, query: QString) {
        crate::orbit_qt::search_library(query.to_string());
    }
}
