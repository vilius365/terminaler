use clap::Parser;
use mux::pane::PaneId;
use terminaler_client::client::Client;

#[derive(Debug, Parser, Clone)]
pub struct Notify {
    /// Title for the notification.
    #[arg(long)]
    title: Option<String>,

    /// The notification body text.
    #[arg(long)]
    body: String,

    /// Specify the target pane.
    /// The default is to use the current pane based on the
    /// environment variable TERMINALER_PANE.
    #[arg(long)]
    pane_id: Option<PaneId>,
}

impl Notify {
    pub async fn run(self, client: Client) -> anyhow::Result<()> {
        let pane_id = client.resolve_pane_id(self.pane_id).await?;
        client
            .notify_alert(codec::NotifyAlert {
                pane_id,
                alert: terminaler_term::Alert::ToastNotification {
                    title: self.title,
                    body: self.body,
                    focus: true,
                },
            })
            .await?;
        Ok(())
    }
}
