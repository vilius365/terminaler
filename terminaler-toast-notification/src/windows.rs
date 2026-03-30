#![cfg(windows)]

use crate::ToastNotification as TN;
use xml::escape::escape_str_pcdata;

use windows::core::{Error as WinError, IInspectable, Interface, HSTRING};
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::Win32::Foundation::E_POINTER;
use windows::UI::Notifications::{
    ToastActivatedEventArgs, ToastNotification, ToastNotificationManager,
};

fn unwrap_arg<T>(a: &Option<T>) -> Result<&T, WinError> {
    match a {
        Some(t) => Ok(t),
        None => Err(WinError::new(E_POINTER, HSTRING::from("option is none"))),
    }
}

fn show_notif_impl(toast: TN) -> Result<(), Box<dyn std::error::Error>> {
    log::info!(
        "Toast notification — title: {:?}, message: {:?}",
        toast.title,
        toast.message
    );

    let xml = XmlDocument::new()?;

    let url_actions = if toast.url.is_some() {
        r#"
        <actions>
           <action content="Show" arguments="show" />
        </actions>
        "#
    } else {
        ""
    };

    // Use a single <text hint-wrap="true"> for the body so Windows wraps all
    // lines instead of silently dropping everything past the 3rd <text> element.
    let escaped_body: String = toast
        .message
        .lines()
        .map(|line| escape_str_pcdata(line).to_string())
        .collect::<Vec<_>>()
        .join("&#xA;");

    xml.LoadXml(HSTRING::from(format!(
        r#"<toast duration="long">
        <visual>
            <binding template="ToastGeneric">
                <text>{}</text>
                <text hint-wrap="true" hint-maxLines="8">{}</text>
            </binding>
        </visual>
        {}
    </toast>"#,
        escape_str_pcdata(&toast.title),
        escaped_body,
        url_actions
    )))?;

    let notif = ToastNotification::CreateToastNotification(xml)?;

    // Save for fallback before toast is moved into closure
    let fallback_title = toast.title.clone();
    let fallback_message = toast.message.clone();

    notif.Failed(&TypedEventHandler::new(
        |_sender: &Option<ToastNotification>,
         result: &Option<windows::UI::Notifications::ToastFailedEventArgs>| {
            if let Some(result) = result {
                log::error!("Toast notification failed: {:?}", result.ErrorCode());
            }
            Ok(())
        },
    ))?;

    notif.Activated(&TypedEventHandler::new(
        move |_: &Option<ToastNotification>, result: &Option<IInspectable>| {
            let result = unwrap_arg(result)?.cast::<ToastActivatedEventArgs>()?;

            let args = result.Arguments()?;

            if args == "show" {
                if let Some(url) = toast.url.as_ref() {
                    let _ = std::process::Command::new("cmd")
                        .args(["/C", "start", "", url])
                        .spawn();
                }
            }

            Ok(())
        },
    ))?;

    let notifier = match ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(
        "org.wezfurlong.terminaler",
    )) {
        Ok(n) => n,
        Err(err) => {
            log::warn!(
                "CreateToastNotifierWithId failed (app not registered?), \
                 trying PowerShell fallback: {:#}",
                err
            );
            // Fallback: use PowerShell for a simple balloon notification
            let _ = std::process::Command::new("powershell")
                .args([
                    "-WindowStyle", "Hidden",
                    "-Command",
                    &format!(
                        "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null; \
                         $xml = [Windows.Data.Xml.Dom.XmlDocument]::new(); \
                         $xml.LoadXml('<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>'); \
                         $toast = [Windows.UI.Notifications.ToastNotification]::new($xml); \
                         [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('Terminaler').Show($toast)",
                        escape_str_pcdata(&fallback_title),
                        escape_str_pcdata(&fallback_message),
                    ),
                ])
                .spawn();
            return Ok(());
        }
    };

    notifier.Show(&notif)?;

    Ok(())
}

pub fn show_notif(notif: TN) -> Result<(), Box<dyn std::error::Error>> {
    // We need to be in a different thread from the caller
    // in case we get called in the guts of a windows message
    // loop dispatch and are unable to pump messages
    std::thread::spawn(move || {
        if let Err(err) = show_notif_impl(notif) {
            log::error!("Failed to show toast notification: {:#}", err);
        }
    });

    Ok(())
}
