//! The ScreenCast portal: the user picks a screen or window in a system dialog and we get a
//! PipeWire stream of just that. The choice is remembered (a restore token) until the user
//! picks again, so later sessions start without the dialog.

use std::os::fd::OwnedFd;
use std::path::PathBuf;

use ashpd::desktop::screencast::{
    CursorMode, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions, SourceType, StartCastOptions,
};
use ashpd::desktop::{CreateSessionOptions, PersistMode, Session};

use crate::i18n::tr;

/// A running screencast: the PipeWire connection and node to read from.
pub struct Cast {
    pub fd: OwnedFd,
    pub node: u32,
    session: Session<Screencast>,
}

impl Drop for Cast {
    fn drop(&mut self) {
        // Ends the screencast (and the desktop's "sharing" indicator) right away.
        let _ = pollster::block_on(self.session.close());
    }
}

fn token_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("obs-lite").join("screencast-token"))
}

/// Asks the portal for a stream; shows the dialog unless an earlier choice can be restored.
pub fn start(cursor: bool) -> Result<Cast, String> {
    let token = token_path().and_then(|p| std::fs::read_to_string(p).ok());
    let result = pollster::block_on(async {
        let proxy = Screencast::new().await?;
        let session = proxy.create_session(CreateSessionOptions::default()).await?;
        let cursor = if cursor { CursorMode::Embedded } else { CursorMode::Hidden };
        let options = SelectSourcesOptions::default()
            .set_cursor_mode(cursor)
            .set_sources(SourceType::Monitor | SourceType::Window)
            .set_multiple(false)
            .set_restore_token(token.as_deref())
            .set_persist_mode(PersistMode::ExplicitlyRevoked);
        proxy.select_sources(&session, options).await?;
        let streams = proxy.start(&session, None, StartCastOptions::default()).await?.response()?;
        let node = streams.streams().first().map(|s| s.pipe_wire_node_id());
        let new_token = streams.restore_token().map(str::to_owned);
        let fd = proxy.open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default()).await?;
        Ok::<_, ashpd::Error>((session, fd, node, new_token))
    });
    let (session, fd, node, new_token) = result.map_err(|e| match e {
        ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled) => {
            tr!("se canceló la elección de pantalla o ventana", "choosing a screen or window was cancelled").to_owned()
        }
        e => e.to_string(),
    })?;
    // Restore tokens are single use: keep the new one for next time.
    if let (Some(token), Some(path)) = (new_token, token_path()) {
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(&path));
        let _ = std::fs::write(path, token);
    }
    let node = node.ok_or_else(|| tr!("el portal no devolvió ninguna imagen", "the portal returned no stream").to_owned())?;
    Ok(Cast { fd, node, session })
}

/// Forgets the remembered choice, so the next session asks again.
pub fn forget() {
    if let Some(path) = token_path() {
        let _ = std::fs::remove_file(path);
    }
}
