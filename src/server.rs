use std::net::SocketAddr;
use std::time::Duration;

use crate::rpc::{dcs, HookRpc, MissionRpc};
use crate::shutdown::{Shutdown, ShutdownHandle};
use dcs::atmosphere_server::AtmosphereServer;
use dcs::coalitions_server::CoalitionsServer;
use dcs::controllers_server::ControllersServer;
use dcs::custom_server::CustomServer;
use dcs::hook_server::HookServer;
use dcs::mission_server::MissionServer;
use dcs::timer_server::TimerServer;
use dcs::triggers_server::TriggersServer;
use dcs::units_server::UnitsServer;
use dcs::world_server::WorldServer;
use dcs::*;
use dcs_module_ipc::IPC;
use futures_util::FutureExt;
use tokio::runtime::Runtime;
use tokio::sync::oneshot::{self, Receiver};
use tokio::time::sleep;
use tonic::transport;

pub struct Server {
    addr: SocketAddr,
    pub ipc_mission: IPC<Event>,
    pub ipc_hook: IPC<()>,
    pub runtime: Runtime,
    shutdown: Shutdown,
    after_shutdown: Option<oneshot::Sender<()>>,
}

impl Server {
    pub fn new(host: &str, port: u16) -> Result<Self, StartError> {
        let ipc_mission = IPC::new();
        let ipc_hook = IPC::new();
        let runtime = Runtime::new()?;
        let shutdown = Shutdown::new();
        Ok(Self {
            addr: format!("{}:{}", host, port).parse()?,
            ipc_mission,
            ipc_hook,
            runtime,
            shutdown,
            after_shutdown: None,
        })
    }

    pub fn run_in_background(&mut self) {
        if self.after_shutdown.is_some() {
            // already running
            return;
        }

        let (tx, rx) = oneshot::channel();
        self.after_shutdown = Some(tx);

        self.runtime.spawn(crate::server::run(
            self.addr,
            self.ipc_mission.clone(),
            self.ipc_hook.clone(),
            self.shutdown.handle(),
            rx,
        ));
    }

    pub fn stop_blocking(mut self) {
        // graceful shutdown
        self.runtime.block_on(self.shutdown.shutdown());
        if let Some(after_shutdown) = self.after_shutdown.take() {
            let _ = after_shutdown.send(());
        }

        // shutdown the async runtime, again give everything another 5 secs before forecefully
        // killing everything
        self.runtime.shutdown_timeout(Duration::from_secs(5));
    }
}

async fn run(
    addr: SocketAddr,
    ipc_mission: IPC<Event>,
    ipc_hook: IPC<()>,
    shutdown_signal: ShutdownHandle,
    mut after_shutdown: Receiver<()>,
) {
    loop {
        match try_run(
            addr,
            ipc_mission.clone(),
            ipc_hook.clone(),
            shutdown_signal.clone(),
            &mut after_shutdown,
        )
        .await
        {
            Ok(_) => break,
            Err(err) => {
                log::error!("{}", err);
                log::info!("Restarting gIPC Server in 10 seconds ...");
                sleep(Duration::from_secs(10)).await;
            }
        }
    }
}

async fn try_run(
    addr: SocketAddr,
    ipc_mission: IPC<Event>,
    ipc_hook: IPC<()>,
    shutdown_signal: ShutdownHandle,
    after_shutdown: &mut Receiver<()>,
) -> Result<(), transport::Error> {
    log::info!("Staring gRPC Server ...");

    let mission_rpc = MissionRpc::new(ipc_mission, shutdown_signal.clone());
    let hook_rpc = HookRpc::new(ipc_hook, shutdown_signal.clone());
    transport::Server::builder()
        .add_service(AtmosphereServer::new(mission_rpc.clone()))
        .add_service(CoalitionsServer::new(mission_rpc.clone()))
        .add_service(ControllersServer::new(mission_rpc.clone()))
        .add_service(CustomServer::new(mission_rpc.clone()))
        .add_service(HookServer::new(hook_rpc))
        .add_service(MissionServer::new(mission_rpc.clone()))
        .add_service(TimerServer::new(mission_rpc.clone()))
        .add_service(TriggersServer::new(mission_rpc.clone()))
        .add_service(UnitsServer::new(mission_rpc.clone()))
        .add_service(WorldServer::new(mission_rpc))
        .serve_with_shutdown(addr, after_shutdown.map(|_| ()))
        .await?;

    log::info!("Server stopped ...");

    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    AddrParse(#[from] std::net::AddrParseError),
}
