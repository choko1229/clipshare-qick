//! Windows 通知トースト（画面 16）。
//!
//! - 連続撮影の完了通知は同じタグで置き換え、「3件のURLをコピーしました」にまとめる（スパム防止）。
//! - ボタン・本文のクリックは、アプリが動いている間はこのプロセスで受け取り、UI スレッドへの命令にする。
//! - パッケージ版はパッケージの AUMID を使う。開発版（パッケージなし）は HKCU に AUMID を登録して使う
//!   （`CQ_DEV_NO_SYSTEM_CHANGES=1` のときは登録せず、通知も出さない）。

use std::sync::{Arc, Mutex, OnceLock};

use cq_core::engine::EngineEvent;
use cq_core::history::EntrySource;
use cq_core::media::MediaKind;
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{
    ToastActivatedEventArgs, ToastNotification, ToastNotificationManager, ToastNotifier,
};
use windows::core::{HSTRING, IInspectable, Interface};

use crate::app::{Command, Screen, Shared};

const DEV_AUMID: &str = "ClipshareQuick.Dev";
const GROUP: &str = "cq";

/// 通知に付けるボタン。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastButton {
    pub label: &'static str,
    pub action: ToastAction,
}

/// クリック時の動作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToastAction {
    OpenUrl(String),
    Delete(String),
    History,
    Login,
    Resume,
    Update,
    ManagePage,
    Contact,
    Dismiss,
}

impl ToastAction {
    fn encode(&self) -> String {
        match self {
            Self::OpenUrl(u) => format!("open:{u}"),
            Self::Delete(id) => format!("delete:{id}"),
            Self::History => "history".into(),
            Self::Login => "login".into(),
            Self::Resume => "resume".into(),
            Self::Update => "update".into(),
            Self::ManagePage => "manage".into(),
            Self::Contact => "contact".into(),
            Self::Dismiss => "dismiss".into(),
        }
    }

    fn decode(s: &str) -> Option<Self> {
        Some(match s {
            "history" => Self::History,
            "login" => Self::Login,
            "resume" => Self::Resume,
            "update" => Self::Update,
            "manage" => Self::ManagePage,
            "contact" => Self::Contact,
            "dismiss" => Self::Dismiss,
            _ => {
                if let Some(u) = s.strip_prefix("open:") {
                    Self::OpenUrl(u.to_owned())
                } else {
                    Self::Delete(s.strip_prefix("delete:")?.to_owned())
                }
            }
        })
    }

    fn into_command(self) -> Option<Command> {
        Some(match self {
            Self::OpenUrl(u) => Command::OpenUrl(u),
            Self::Delete(id) => Command::DeleteEntry(id),
            Self::History => Command::Open(Screen::History),
            Self::Login => Command::Login,
            Self::Resume => Command::SetAutoUpload(true),
            Self::Update => Command::Open(Screen::Update),
            Self::ManagePage => Command::OpenManagePage,
            Self::Contact => Command::OpenContactPage,
            Self::Dismiss => return None,
        })
    }
}

/// 通知の内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToastSpec {
    pub title: String,
    pub body: String,
    pub buttons: Vec<ToastButton>,
    /// 同じタグの通知は置き換える。
    pub tag: Option<&'static str>,
    pub launch: ToastAction,
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// トーストの XML を作る。
pub fn to_xml(spec: &ToastSpec) -> String {
    let mut x = format!(
        "<toast launch=\"{}\"><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual>",
        xml_escape(&spec.launch.encode()),
        xml_escape(&spec.title),
        xml_escape(&spec.body)
    );
    if !spec.buttons.is_empty() {
        x.push_str("<actions>");
        for b in &spec.buttons {
            x.push_str(&format!(
                "<action content=\"{}\" arguments=\"{}\" activationType=\"foreground\"/>",
                xml_escape(b.label),
                xml_escape(&b.action.encode())
            ));
        }
        x.push_str("</actions>");
    }
    x.push_str("</toast>");
    x
}

struct Toaster {
    notifier: ToastNotifier,
    /// クリックを受け取るため、表示中の通知を保持する（古いものから捨てる）。
    alive: Mutex<Vec<ToastNotification>>,
}

// SAFETY: ToastNotifier / ToastNotification はアジャイルな WinRT オブジェクトで、どのスレッドからも使える。
unsafe impl Send for Toaster {}
// SAFETY: 同上。内部の可変状態は Mutex で保護している。
unsafe impl Sync for Toaster {}

static TOASTER: OnceLock<Option<Toaster>> = OnceLock::new();

fn register_dev_aumid() -> bool {
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
        RegCreateKeyExW, RegSetValueExW,
    };
    use windows::core::{PCWSTR, w};
    let sub = HSTRING::from(format!(r"Software\Classes\AppUserModelId\{DEV_AUMID}"));
    let mut key = HKEY::default();
    // SAFETY: 作成したキーは下で閉じる。
    let ok = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(sub.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
    }
    .is_ok();
    if !ok {
        return false;
    }
    let name: Vec<u16> = "Clipshare Quick".encode_utf16().chain(Some(0)).collect();
    // SAFETY: NUL 終端のワイド文字列をバイト列として渡す。
    let r = unsafe {
        RegSetValueExW(
            key,
            w!("DisplayName"),
            None,
            REG_SZ,
            Some(std::slice::from_raw_parts(
                name.as_ptr().cast::<u8>(),
                name.len() * 2,
            )),
        )
    };
    // SAFETY: 開いたキーを閉じる。
    let _ = unsafe { RegCloseKey(key) };
    r.is_ok()
}

/// 開発版で登録した AUMID を消す（アンインストール時）。
pub fn unregister() {
    if crate::platform::is_packaged() || crate::platform::system_changes_disabled() {
        return;
    }
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteTreeW};
    use windows::core::PCWSTR;
    let sub = HSTRING::from(format!(r"Software\Classes\AppUserModelId\{DEV_AUMID}"));
    // SAFETY: HKCU 配下の自分のキーだけを消す。
    let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(sub.as_ptr())) };
}

fn toaster() -> Option<&'static Toaster> {
    TOASTER
        .get_or_init(|| {
            let notifier = if crate::platform::is_packaged() {
                ToastNotificationManager::CreateToastNotifier()
            } else {
                if crate::platform::system_changes_disabled() || !register_dev_aumid() {
                    tracing::info!("開発モードのため Windows 通知を出しません");
                    return None;
                }
                ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(DEV_AUMID))
            };
            match notifier {
                Ok(n) => Some(Toaster {
                    notifier: n,
                    alive: Mutex::new(Vec::new()),
                }),
                Err(e) => {
                    tracing::warn!(error = %e, "通知を初期化できませんでした");
                    None
                }
            }
        })
        .as_ref()
}

/// 通知を出す。
pub fn show(shared: &Arc<Shared>, spec: &ToastSpec) {
    if !shared.engine.settings().get().notifications.windows_toast {
        return;
    }
    let Some(t) = toaster() else { return };
    if let Err(e) = show_inner(shared, t, spec) {
        tracing::warn!(error = %e, "通知を表示できませんでした");
    }
}

fn show_inner(shared: &Arc<Shared>, t: &Toaster, spec: &ToastSpec) -> windows::core::Result<()> {
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(to_xml(spec)))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    if let Some(tag) = spec.tag {
        toast.SetTag(&HSTRING::from(tag))?;
        toast.SetGroup(&HSTRING::from(GROUP))?;
    }
    let s = Arc::clone(shared);
    toast.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(
        move |_, args| {
            let Some(args) = args.as_ref() else {
                return Ok(());
            };
            let a: ToastActivatedEventArgs = args.cast()?;
            let arg = a.Arguments()?.to_string();
            if let Some(cmd) = ToastAction::decode(&arg).and_then(ToastAction::into_command) {
                s.send(cmd);
            }
            Ok(())
        },
    ))?;
    t.notifier.Show(&toast)?;
    if let Ok(mut g) = t.alive.lock() {
        g.push(toast);
        if g.len() > 20 {
            g.remove(0);
        }
    }
    Ok(())
}

/// ボタンなしの通知。
pub fn show_simple(shared: &Arc<Shared>, title: &str, body: &str) {
    show(
        shared,
        &ToastSpec {
            title: title.to_owned(),
            body: body.to_owned(),
            buttons: vec![],
            tag: None,
            launch: ToastAction::History,
        },
    );
}

/// エンジンのイベントを通知の内容にする（表示しないものは `None`）。
pub fn spec_for(ev: &EngineEvent) -> Option<ToastSpec> {
    let spec = |title: String,
                body: String,
                buttons: Vec<ToastButton>,
                tag: Option<&'static str>| ToastSpec {
        title,
        body,
        buttons,
        tag,
        launch: ToastAction::History,
    };
    Some(match ev {
        EngineEvent::Uploaded {
            entry_id,
            url,
            source,
            processing,
            copied,
            copied_count,
            kind,
            ..
        } if *source == EntrySource::Auto || *copied => {
            if *kind == MediaKind::Video && *processing {
                let body = if *copied {
                    "URLはコピー済みです。変換が終わると再生できます。"
                } else {
                    "変換が終わると再生できます。"
                };
                spec(
                    "動画を処理しています".into(),
                    body.into(),
                    vec![],
                    Some("upload"),
                )
            } else if *copied && *copied_count > 1 {
                spec(
                    format!("{copied_count}件のURLをコピーしました"),
                    "改行区切りで貼り付けられます。".into(),
                    vec![ToastButton {
                        label: "履歴を開く",
                        action: ToastAction::History,
                    }],
                    Some("upload"),
                )
            } else {
                let title = if *copied {
                    "共有URLをコピーしました"
                } else {
                    "アップロードしました"
                };
                spec(
                    title.into(),
                    format!("{} · 24時間有効", crate::ui::history::short_url(url)),
                    vec![
                        ToastButton {
                            label: "開く",
                            action: ToastAction::OpenUrl(url.clone()),
                        },
                        ToastButton {
                            label: "削除",
                            action: ToastAction::Delete(entry_id.clone()),
                        },
                    ],
                    Some("upload"),
                )
            }
        }
        EngineEvent::Shrunk {
            file_name,
            original_bytes,
        } => spec(
            "サイズが大きいため縮小しました".into(),
            format!(
                "{file_name}（{}）を長辺2000pxに縮小して送りました。",
                cq_core::update::format_size(*original_bytes)
            ),
            vec![],
            None,
        ),
        EngineEvent::VideoTooLarge {
            file_name,
            size_bytes,
        } => spec(
            "サイズが上限を超えています".into(),
            format!(
                "{file_name}（{}）は縮小できないため、アップロードできませんでした。",
                cq_core::update::format_size(*size_bytes)
            ),
            vec![ToastButton {
                label: "閉じる",
                action: ToastAction::Dismiss,
            }],
            None,
        ),
        EngineEvent::DailyLimitReached { anonymous, limit } => {
            let title = match limit {
                Some(l) => format!("今日の上限（{l}件）に達しました"),
                None => "今日の上限に達しました".to_owned(),
            };
            if *anonymous {
                spec(
                    title,
                    "明日から再開します。それまでに撮った写真は送りません。ログインすると上限が上がります。".into(),
                    vec![ToastButton {
                        label: "ログインして連携",
                        action: ToastAction::Login,
                    }],
                    None,
                )
            } else {
                spec(
                    title,
                    "明日から再開します。それまでに撮った写真は送りません。".into(),
                    vec![],
                    None,
                )
            }
        }
        // 403（BAN など）は再認可で解決しないため、お問い合わせページへ案内する（Q-024）。
        // 再ログイン・再試行は促さない。
        EngineEvent::AccountBlocked => ToastSpec {
            title: "このアカウントでは利用できません".into(),
            body: String::new(),
            buttons: vec![ToastButton {
                label: "お問い合わせ",
                action: ToastAction::Contact,
            }],
            tag: Some("blocked"),
            launch: ToastAction::Contact,
        },
        EngineEvent::AuthExpired => spec(
            "連携が切れました".into(),
            // デザインの「バナー付き・1日5件で動作します」は確定仕様（匿名へ切り替えない）と矛盾するため置き換えた（QUESTIONS Q-003）。
            "再ログインするまでアップロードを停止します。".into(),
            vec![ToastButton {
                label: "再ログイン",
                action: ToastAction::Login,
            }],
            None,
        ),
        EngineEvent::Offline { waiting } => spec(
            "アップロードできませんでした".into(),
            format!("接続が戻ると自動で再試行します。{waiting}件が待機中です。"),
            vec![ToastButton {
                label: "履歴を開く",
                action: ToastAction::History,
            }],
            Some("offline"),
        ),
        EngineEvent::AutoUploadChanged { enabled: false } => spec(
            "自動アップロードを一時停止しました".into(),
            "トレイから再開できます。手動の共有は使えます。".into(),
            vec![ToastButton {
                label: "再開",
                action: ToastAction::Resume,
            }],
            Some("auto"),
        ),
        EngineEvent::AutoUploadChanged { enabled: true } => spec(
            "自動アップロードを再開しました".into(),
            String::new(),
            vec![],
            Some("auto"),
        ),
        // 1日上限・連携切れ・動画のサイズ超過は専用の通知を出す。キャンセルは通知しない。
        EngineEvent::Failed {
            file_name,
            message,
            code,
            ..
        } if !matches!(
            code.as_str(),
            "DAILY_LIMIT_EXCEEDED"
                | "CANCELLED"
                | "UNAUTHORIZED"
                | "VIDEO_TOO_LARGE"
                | "FORBIDDEN"
                | "AUTO_UPLOAD_OFF"
        ) =>
        {
            spec(
                "アップロードできませんでした".into(),
                format!("{file_name}: {message}"),
                vec![ToastButton {
                    label: "履歴を開く",
                    action: ToastAction::History,
                }],
                None,
            )
        }
        _ => return None,
    })
}

/// エンジンのイベントを通知に流し続ける。
pub fn spawn_event_loop(shared: &Arc<Shared>) {
    let s = Arc::clone(shared);
    let mut rx = shared.engine.subscribe();
    shared.rt.spawn(async move {
        loop {
            let ev = match rx.recv().await {
                Ok(ev) => ev,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!(skipped = n, "通知の一部を省略しました");
                    continue;
                }
                Err(_) => return,
            };
            // 状態の変化に合わせてトレイを更新させる。
            s.wake();
            if let Some(spec) = spec_for(&ev) {
                show(&s, &spec);
            }
            crate::vr_bridge::on_event(&s, &ev);
            if let EngineEvent::BatchStarted { batch_id } = &ev {
                s.send(Command::Open(Screen::Progress(batch_id.clone())));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_is_escaped_and_has_actions() {
        let x = to_xml(&ToastSpec {
            title: "a<b>&\"".into(),
            body: "c".into(),
            buttons: vec![ToastButton {
                label: "開く",
                action: ToastAction::OpenUrl("https://clipshare.link/q/a?x=1&y=2".into()),
            }],
            tag: None,
            launch: ToastAction::History,
        });
        assert!(x.contains("a&lt;b&gt;&amp;&quot;"));
        assert!(x.contains("arguments=\"open:https://clipshare.link/q/a?x=1&amp;y=2\""));
        assert!(x.starts_with("<toast launch=\"history\">"));
    }

    #[test]
    fn actions_roundtrip() {
        for a in [
            ToastAction::OpenUrl("https://x/q/1".into()),
            ToastAction::Delete("abc".into()),
            ToastAction::History,
            ToastAction::Login,
            ToastAction::Resume,
            ToastAction::Update,
            ToastAction::ManagePage,
            ToastAction::Contact,
            ToastAction::Dismiss,
        ] {
            assert_eq!(ToastAction::decode(&a.encode()), Some(a));
        }
        assert_eq!(ToastAction::decode("rm -rf"), None);
    }

    #[test]
    fn forbidden_points_to_contact_page_only() {
        let s = spec_for(&EngineEvent::AccountBlocked).unwrap();
        assert_eq!(s.title, "このアカウントでは利用できません");
        assert_eq!(s.launch, ToastAction::Contact);
        assert_eq!(s.buttons.len(), 1);
        assert_eq!(s.buttons[0].action, ToastAction::Contact);
        assert_eq!(
            ToastAction::Contact.into_command(),
            Some(Command::OpenContactPage)
        );
        // 再ログイン・再試行は促さない
        assert!(
            !s.buttons
                .iter()
                .any(|b| matches!(b.action, ToastAction::Login))
        );
        // 失敗の通知も別に出さない（同じことを二重に知らせない）
        let failed = EngineEvent::Failed {
            entry_id: "1".into(),
            file_name: "a.png".into(),
            code: "FORBIDDEN".into(),
            message: "x".into(),
        };
        assert!(spec_for(&failed).is_none());
    }

    #[test]
    fn texts_follow_design() {
        let ev = EngineEvent::Uploaded {
            entry_id: "1".into(),
            file_name: "a.png".into(),
            url: "https://clipshare.link/q/aB3dE9xYz1".into(),
            source: EntrySource::Auto,
            processing: false,
            copied: true,
            copied_count: 1,
            kind: MediaKind::Image,
        };
        let s = spec_for(&ev).unwrap();
        assert_eq!(s.title, "共有URLをコピーしました");
        assert_eq!(s.body, "clipshare.link/q/aB3dE9xYz1 · 24時間有効");
        assert_eq!(s.buttons.len(), 2);

        let s = spec_for(&EngineEvent::DailyLimitReached {
            anonymous: true,
            limit: Some(5),
        })
        .unwrap();
        assert_eq!(s.title, "今日の上限（5件）に達しました");
        assert_eq!(
            s.body,
            "明日から再開します。それまでに撮った写真は送りません。ログインすると上限が上がります。"
        );

        let s = spec_for(&EngineEvent::VideoTooLarge {
            file_name: "clip_0412.mp4".into(),
            size_bytes: 320 * 1024 * 1024,
        })
        .unwrap();
        assert_eq!(
            s.body,
            "clip_0412.mp4（320 MB）は縮小できないため、アップロードできませんでした。"
        );
        let s = spec_for(&EngineEvent::Offline { waiting: 3 }).unwrap();
        assert_eq!(s.body, "接続が戻ると自動で再試行します。3件が待機中です。");
        // 右クリック共有の完了は進捗ウィンドウで知らせるため通知しない
        let manual = EngineEvent::Uploaded {
            entry_id: "1".into(),
            file_name: "a.png".into(),
            url: "u".into(),
            source: EntrySource::Manual,
            processing: false,
            copied: false,
            copied_count: 0,
            kind: MediaKind::Image,
        };
        assert!(spec_for(&manual).is_none());
    }
}
