//! "Buy on Qobuz" (2026-09-13): a link to the store page in the browser,
//! never a purchase inside QBZ. The album page shows the action only when the
//! catalog says the release is `purchasable`; the track menus and the player
//! bar resolve that at click time — not every row document carries the flag —
//! and say so in a toast when a release is not sold. The URL keeps the store
//! locale to Qobuz: `/album/-/{id}` answers 301 to the localised page
//! (verified 2026-09-13). Tracks are sold on their album's page.

pub(crate) fn album_store_url(album_id: &str) -> String {
    format!("https://www.qobuz.com/album/-/{album_id}")
}

fn open_store(url: String) {
    if let Err(error) = open::that(&url) {
        log::warn!("[qbz-qt] store link {url} failed to open: {error}");
        crate::toast_qt::error(qbz_i18n::t("Could not open the browser"));
    }
}

fn not_sold() {
    crate::toast_qt::info(qbz_i18n::t("This release isn't sold on Qobuz."));
}

fn unknown() {
    crate::toast_qt::info(qbz_i18n::t("Couldn't check the store for this release."));
}

pub(crate) fn buy_album(album_id: String) {
    if album_id.is_empty() {
        return;
    }
    let runtime = crate::app();
    crate::spawn(async move {
        match runtime.core().get_album(&album_id).await {
            Ok(album) if album.purchasable == Some(false) => not_sold(),
            Ok(_) => open_store(album_store_url(&album_id)),
            Err(error) => {
                log::warn!("[qbz-qt] store: album {album_id} lookup failed: {error}");
                unknown();
            }
        }
    });
}

pub(crate) fn buy_track(track_id: String) {
    let Ok(id) = track_id.parse::<u64>() else {
        return;
    };
    let runtime = crate::app();
    crate::spawn(async move {
        match runtime.core().get_track(id).await {
            Ok(track) if track.purchasable == Some(false) => not_sold(),
            Ok(track) => match track.album.as_ref().map(|a| a.id.clone()) {
                Some(album_id) if !album_id.is_empty() => open_store(album_store_url(&album_id)),
                _ => unknown(),
            },
            Err(error) => {
                log::warn!("[qbz-qt] store: track {id} lookup failed: {error}");
                unknown();
            }
        }
    });
}
