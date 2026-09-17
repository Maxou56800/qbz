//! Experimental library administration on the selected host. These commands
//! first read its identity, then submit an instance-scoped operation. They never
//! open a desktop library or resolve the supplied directory on the controller.
use crate::{
    cli::client::{ApiClient, CliError},
    paths::ProfileRoots,
    LibraryCmd,
};
use qbz_control::library::{LibraryInfo, PROTOCOL_VERSION};
use serde_json::{json, Value};

pub async fn run(host: Option<String>, roots: &ProfileRoots, command: LibraryCmd) -> i32 {
    match execute(&ApiClient::new(host, roots), command).await {
        Ok(value) => {
            println!("{value}");
            0
        }
        Err(error) => {
            eprintln!("{error}");
            error.exit_code()
        }
    }
}

async fn execute(client: &ApiClient, command: LibraryCmd) -> Result<Value, CliError> {
    let value = client.get("/api/orbit/library").await?;
    let info: LibraryInfo = serde_json::from_value(value.clone())
        .map_err(|_| CliError::Runtime("host returned an invalid library identity".into()))?;
    if info.protocol != PROTOCOL_VERSION || info.instance.is_empty() {
        return Err(CliError::Runtime(
            "host uses an unsupported Orbit library protocol".into(),
        ));
    }
    if matches!(command, LibraryCmd::Info) {
        return Ok(value);
    }
    if let LibraryCmd::Search { query, limit } = command {
        if !(1..=100).contains(&limit) {
            return Err(CliError::Runtime(
                "library limit must be between 1 and 100".into(),
            ));
        }
        return client
            .get(&format!(
                "/api/orbit/library/search?instance={}&q={}&limit={limit}",
                urlencoding::encode(&info.instance),
                urlencoding::encode(&query)
            ))
            .await;
    }
    if !info
        .capabilities
        .iter()
        .any(|c| c == "library.files.manage")
    {
        return Err(CliError::Runtime(
            "host does not offer library administration".into(),
        ));
    }
    let instance = urlencoding::encode(&info.instance);
    match command {
        LibraryCmd::Folders => {
            client
                .get(&format!("/api/orbit/library/folders?instance={instance}"))
                .await
        }
        LibraryCmd::Jobs => {
            client
                .get(&format!("/api/orbit/library/jobs?instance={instance}"))
                .await
        }
        LibraryCmd::Add { path } => {
            client
                .post(
                    "/api/orbit/library/folders",
                    json!({"instance":info.instance, "revision":info.revision, "path":path}),
                )
                .await
        }
        LibraryCmd::Scan { folder } => {
            client
                .post(
                    "/api/orbit/library/scan",
                    json!({"instance":info.instance, "folder_id":folder}),
                )
                .await
        }
        LibraryCmd::Cancel { job } => {
            client
                .post(
                    "/api/orbit/library/cancel",
                    json!({"instance":info.instance, "job":job}),
                )
                .await
        }
        LibraryCmd::Info | LibraryCmd::Search { .. } => unreachable!(),
    }
}
