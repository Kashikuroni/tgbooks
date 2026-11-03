use std::{error::Error, future::Future};

use async_channel::{Receiver, Sender};

use crate::DownloadRecord;

/// This port is implemented by storage. Tests can supply an in-memory adapter.
pub trait DownloadHistory: Send + Sync {
    type Error: Error + Send + Sync + 'static;

    fn list_downloads(
        &self,
    ) -> impl Future<Output = Result<Vec<DownloadRecord>, Self::Error>> + Send;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreCommand {
    RefreshHistory,
    Shutdown,
}

/// Display-only paths supplied by the platform adapter, not resolved by Core.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppInfo {
    pub data_directory: String,
    pub download_directory: String,
}

/// Only safe, typed errors cross the UI boundary, never raw API/SQL errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceFailure {
    Initialization,
    Storage,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CoreEvent {
    Ready(AppInfo),
    HistoryLoaded(Vec<DownloadRecord>),
    Failed(ServiceFailure),
}

/// Both channel ends work on Tokio and GPUI's executor, without polling timers.
pub struct UiConnection {
    pub commands: Sender<CoreCommand>,
    pub events: Receiver<CoreEvent>,
}

pub struct ServiceConnection {
    pub commands: Receiver<CoreCommand>,
    pub events: Sender<CoreEvent>,
}

pub fn connect() -> (UiConnection, ServiceConnection) {
    let (commands_tx, commands_rx) = async_channel::bounded(16);
    let (events_tx, events_rx) = async_channel::bounded(16);
    (
        UiConnection {
            commands: commands_tx,
            events: events_rx,
        },
        ServiceConnection {
            commands: commands_rx,
            events: events_tx,
        },
    )
}

async fn load_history(history: &impl DownloadHistory, events: &Sender<CoreEvent>) -> bool {
    let event = match history.list_downloads().await {
        Ok(records) => CoreEvent::HistoryLoaded(records),
        Err(_) => CoreEvent::Failed(ServiceFailure::Storage),
    };
    events.send(event).await.is_ok()
}

/// One owner processes commands in order. Dropping the UI closes its command
/// sender and event receiver; closing commands explicitly also stops the loop.
pub async fn run_service(
    history: &impl DownloadHistory,
    info: AppInfo,
    connection: ServiceConnection,
) {
    if connection
        .events
        .send(CoreEvent::Ready(info))
        .await
        .is_err()
        || !load_history(history, &connection.events).await
    {
        return;
    }

    while let Ok(command) = connection.commands.recv().await {
        match command {
            CoreCommand::RefreshHistory => {
                if !load_history(history, &connection.events).await {
                    break;
                }
            }
            CoreCommand::Shutdown => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{convert::Infallible, io};

    use super::*;

    struct EmptyHistory;

    impl DownloadHistory for EmptyHistory {
        type Error = Infallible;

        async fn list_downloads(&self) -> Result<Vec<DownloadRecord>, Self::Error> {
            Ok(Vec::new())
        }
    }

    struct BrokenHistory;

    impl DownloadHistory for BrokenHistory {
        type Error = io::Error;

        async fn list_downloads(&self) -> Result<Vec<DownloadRecord>, Self::Error> {
            Err(io::Error::other("private database details"))
        }
    }

    fn info() -> AppInfo {
        AppInfo {
            data_directory: "data".into(),
            download_directory: "books".into(),
        }
    }

    #[tokio::test]
    async fn commands_produce_ordered_events_and_shutdown() -> Result<(), Box<dyn Error>> {
        let (ui, backend) = connect();
        let task = tokio::spawn(async { run_service(&EmptyHistory, info(), backend).await });
        assert_eq!(ui.events.recv().await?, CoreEvent::Ready(info()));
        assert_eq!(
            ui.events.recv().await?,
            CoreEvent::HistoryLoaded(Vec::new())
        );
        ui.commands.send(CoreCommand::RefreshHistory).await?;
        assert_eq!(
            ui.events.recv().await?,
            CoreEvent::HistoryLoaded(Vec::new())
        );
        ui.commands.send(CoreCommand::Shutdown).await?;
        task.await?;
        assert!(ui.events.recv().await.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn adapter_errors_are_not_exposed_to_ui() -> Result<(), Box<dyn Error>> {
        let (ui, backend) = connect();
        let task = tokio::spawn(async { run_service(&BrokenHistory, info(), backend).await });
        assert_eq!(ui.events.recv().await?, CoreEvent::Ready(info()));
        assert_eq!(
            ui.events.recv().await?,
            CoreEvent::Failed(ServiceFailure::Storage)
        );
        drop(ui);
        task.await?;
        Ok(())
    }

    #[tokio::test]
    async fn disconnected_ui_stops_the_service() -> Result<(), Box<dyn Error>> {
        let (ui, backend) = connect();
        drop(ui);
        run_service(&EmptyHistory, info(), backend).await;
        Ok(())
    }
}
