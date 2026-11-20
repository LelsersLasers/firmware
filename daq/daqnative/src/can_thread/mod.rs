//! Caller-managed worker. Only commands/events cross threads; telemetry stays with callers.

use std::{io, path, sync::mpsc, thread};

mod connection;
mod decode;
mod events;
mod firmware_session;
mod run;
pub mod tx;
use crate::{can, can_thread, firmware, frame, hil, superdbc, time};

pub use daqcore::superdbc::{BusSelection, DatabaseSelection, DatabaseSource};

/// A file replacement loads on the worker.
/// A bus change reuses its active database.
#[derive(Debug, Clone)]
pub enum DatabaseRequest {
    Load {
        path: path::PathBuf,
        preferred_selection: Option<BusSelection>,
    },
    Bus {
        selection: BusSelection,
    },
}

/// Successful selection, sent only after the worker applies the new context.
#[derive(Debug)]
pub enum DatabaseUpdate {
    Loaded {
        database: Box<superdbc::database::Database>,
        selection: BusSelection,
    },
    Bus {
        selection: BusSelection,
    },
}

#[derive(Default)]
pub struct CanThreadConfig {
    pub log_folder: Option<path::PathBuf>,
    pub hil_dir: path::PathBuf,
}

pub enum CanThreadCommand {
    Connect(Option<crate::connection::ConnectionSource>),
    SelectDatabase {
        request_id: u64,
        selection: DatabaseRequest,
    },
    AddSendMessage(can_thread::tx::AddSendMessage),
    DeleteSendMessage {
        key: superdbc::message::MessageKey,
    },
    UpdateLogFolder(path::PathBuf),
    Stop,
    Hil(hil::engine::HilCommand),
    StartFirmwareUpdate(firmware::protocol::FirmwarePackage),
    ArmFirmwareUpdate(firmware::protocol::FirmwarePackage),
    CancelFirmwareUpdate,
}

pub enum CanThreadEvent {
    DatabaseSelected {
        request_id: u64,
        result: Result<DatabaseUpdate, String>,
    },
    Frame(frame::ParsedFrame),
    SourceSelected(Option<crate::connection::ConnectionSource>),
    Disconnection,
    ConnectionSuccessful,
    ConnectionFailed(String),
    MessageSent {
        key: superdbc::message::MessageKey,
        timestamp: time::Time,
        amount_left: Option<can_thread::tx::SendAmount>,
    },
    SendFailed {
        key: superdbc::message::MessageKey,
        error: String,
        retrying: bool,
    },
    Diagnostic(String),
    BusLoad {
        timestamp: time::Time,
        buses: Vec<can::bus_load::BusLoadReading>,
    },
    Hil(hil::engine::HilSnapshot),
    FirmwareProgress(firmware::FirmwareProgress),
}

/// Single caller owns shutdown; cloned senders submit commands but do not own the worker.
pub struct CanThreadHandle {
    tx: mpsc::Sender<CanThreadCommand>,
    join: Option<thread::JoinHandle<()>>,
}

impl CanThreadHandle {
    pub fn command(
        &self,
        command: CanThreadCommand,
    ) -> Result<(), mpsc::SendError<CanThreadCommand>> {
        self.tx.send(command)
    }

    pub fn sender(&self) -> mpsc::Sender<CanThreadCommand> {
        self.tx.clone()
    }

    pub fn stop(&mut self) -> thread::Result<()> {
        let _ = self.tx.send(CanThreadCommand::Stop);
        match self.join.take() {
            Some(thread) => thread.join(),
            None => Ok(()),
        }
    }
}

impl Drop for CanThreadHandle {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub fn spawn_can_thread(
    config: CanThreadConfig,
    out: mpsc::Sender<CanThreadEvent>,
) -> io::Result<CanThreadHandle> {
    let (tx, rx) = mpsc::channel();
    let join = thread::Builder::new()
        .name("daq-can".into())
        .spawn(move || run::run(config, out, rx))?;
    Ok(CanThreadHandle {
        tx,
        join: Some(join),
    })
}
