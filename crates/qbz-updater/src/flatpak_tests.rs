use super::*;
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::Arc,
};

struct Bus(Child, String);
impl Bus {
    fn new() -> Self {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("dbus-daemon is required for the updater wire tests");
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        Self(child, address.trim().into())
    }
    async fn connect(&self) -> Connection {
        zbus::connection::Builder::address(self.1.as_str())
            .unwrap()
            .build()
            .await
            .unwrap()
    }
}
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Portal {
    closed: Arc<AtomicBool>,
    status: u32,
}
struct UpdateObject {
    closed: Arc<AtomicBool>,
    status: u32,
}
#[zbus::interface(name = "org.freedesktop.portal.Flatpak")]
impl Portal {
    async fn create_update_monitor(
        &self,
        options: Dict,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<OwnedObjectPath> {
        let token = <&str>::try_from(options.get("handle_token").unwrap()).unwrap();
        let sender = header
            .sender()
            .unwrap()
            .as_str()
            .trim_start_matches(':')
            .replace('.', "_");
        let path = format!("{ROOT}/update_monitor/{sender}/{token}");
        connection
            .object_server()
            .at(
                path.clone(),
                UpdateObject {
                    closed: self.closed.clone(),
                    status: self.status,
                },
            )
            .await
            .unwrap();
        let info = HashMap::from([
            ("running-commit", Value::from("a".repeat(64))),
            ("local-commit", Value::from("a".repeat(64))),
            ("remote-commit", Value::from("b".repeat(64))),
        ]);
        // Intentionally send before the method reply: subscriptions installed
        // afterwards lose this cached initial notification.
        connection
            .emit_signal(
                header.sender().cloned(),
                path.as_str(),
                INTERFACE,
                "UpdateAvailable",
                &(info,),
            )
            .await
            .unwrap();
        Ok(OwnedObjectPath::try_from(path).unwrap())
    }
}
#[zbus::interface(name = "org.freedesktop.portal.Flatpak.UpdateMonitor")]
impl UpdateObject {
    async fn update(
        &self,
        parent_window: &str,
        options: Dict,
        #[zbus(connection)] connection: &Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        assert!(parent_window.is_empty());
        assert!(options.is_empty());
        if self.status == 0 {
            return Ok(());
        }
        let progress = HashMap::from([("status", Value::from(self.status))]);
        connection
            .emit_signal(
                header.sender().cloned(),
                header.path().unwrap(),
                INTERFACE,
                "Progress",
                &(progress,),
            )
            .await
            .unwrap();
        Ok(())
    }
    fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
    }
}

async fn fixture(status: u32) -> (Bus, Connection, Arc<AtomicBool>, Monitor) {
    let bus = Bus::new();
    let closed = Arc::new(AtomicBool::new(false));
    let server = zbus::connection::Builder::address(bus.1.as_str())
        .unwrap()
        .name(SERVICE)
        .unwrap()
        .serve_at(
            ROOT,
            Portal {
                closed: closed.clone(),
                status,
            },
        )
        .unwrap()
        .build()
        .await
        .unwrap();
    let monitor = tokio::time::timeout(
        Duration::from_secs(5),
        Monitor::with_connection(bus.connect().await),
    )
    .await
    .unwrap()
    .unwrap();
    (bus, server, closed, monitor)
}

#[tokio::test]
async fn early_portal_signals_survive_check_and_install() {
    let (_bus, _server, closed, monitor) = fixture(2).await;
    assert_eq!(monitor.info.availability(), Availability::Available);
    let cancel = AtomicBool::new(false);
    tokio::time::timeout(Duration::from_secs(5), monitor.install(&cancel, |_| {}))
        .await
        .unwrap()
        .unwrap();
    assert!(closed.load(Ordering::Relaxed));
}
#[tokio::test]
async fn portal_failure_is_reported_and_monitor_closed() {
    let (_bus, _server, closed, monitor) = fixture(3).await;
    let cancel = AtomicBool::new(false);
    let result = tokio::time::timeout(Duration::from_secs(5), monitor.install(&cancel, |_| {}))
        .await
        .unwrap();
    assert!(result.is_err());
    assert!(closed.load(Ordering::Relaxed));
}
#[tokio::test]
async fn cancelling_portal_update_closes_the_native_transaction() {
    let (_bus, _server, closed, monitor) = fixture(0).await;
    let cancel = AtomicBool::new(true);
    let result = tokio::time::timeout(Duration::from_secs(5), monitor.install(&cancel, |_| {}))
        .await
        .unwrap();
    assert!(result.unwrap_err().contains("cancelled"));
    assert!(closed.load(Ordering::Relaxed));
}

#[tokio::test]
async fn an_empty_transaction_never_reports_a_successful_installation() {
    let (_bus, _server, closed, monitor) = fixture(1).await;
    let cancel = AtomicBool::new(false);
    let result = tokio::time::timeout(Duration::from_secs(5), monitor.install(&cancel, |_| {}))
        .await.unwrap();
    assert!(result.unwrap_err().contains("no update to install"));
    assert!(closed.load(Ordering::Relaxed));
}
