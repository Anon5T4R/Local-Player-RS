//! i18n via craft-i18n (padrão storytold): catálogos TSV em `i18n/locales/` embutidos
//! pelo `build.rs` (fonte: `scripts/gen_i18n.py` converte este enum). O enum `Key`
//! fica como API tipada (compilador cobre completude); o lookup é o motor do
//! craft-i18n, com fallback `pt-br → pt → en` e ausência de tradução = inglês.

use std::sync::LazyLock;

use craft_i18n::I18n;

include!(concat!(env!("OUT_DIR"), "/craft_i18n.rs"));

/// Códigos dos idiomas oferecidos (mesmos do enum `Lang`).
const OFFERED: &[&str] = &["en", "pt-br", "es"];

static I18N: LazyLock<I18n> = LazyLock::new(|| {
    let (i18n, problems) = I18n::new(CRAFT_I18N, OFFERED);
    for p in problems {
        eprintln!("lab-ui i18n: {p}");
    }
    i18n
});

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum Lang {
    Pt,
    En,
    Es,
}

impl Lang {
    pub const ALL: [Lang; 3] = [Lang::Pt, Lang::En, Lang::Es];

    /// Nome no seletor — sempre endônimo (regra da suíte).
    pub fn label(self) -> &'static str {
        match self {
            Lang::Pt => "Português",
            Lang::En => "English",
            Lang::Es => "Español",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    // compartilhadas
    Language,
    Theme,
    Error,
    Search,
    Copy,
    // lab-monitor
    Cpu,
    Memory,
    Cores,
    Uptime,
    Disks,
    Processes,
    Filter,
    Name,
    End,
    KillTitle,
    KillAsk,
    Confirm,
    Cancel,
    Clear,
    // lab-clip
    ShowHide,
    Quit,
    Pin,
    Unpin,
    Delete,
    Empty,
    // lab-keys
    Vault,
    MasterPassword,
    Unlock,
    NewVault,
    WrongPassword,
    Add,
    Save,
    Items,
    Username,
    Password,
    Lock,
    // lab-keys onda 4
    Totp,
    QuickUnlock,
    ForgetKey,
    Edit,
    SecondsShort,
    // lab-keys: confirmação de exclusão
    TrashHint,
    // lab-keys onda 6 — importação, gerador, anexos, senha mestra e auto-type
    Import,
    ImportKdbx,
    ImportBitwarden,
    ImportCsv,
    ImportFile,
    ImportPassword,
    Imported,
    SkippedCardIdentity,
    ImportReportTitle,
    Generator,
    Generate,
    Use,
    Passphrase,
    Words,
    Separator,
    Capitalize,
    IncludeNumber,
    Length,
    Lowercase,
    Uppercase,
    Digits,
    Symbols,
    Strength0,
    Strength1,
    Strength2,
    Strength3,
    Strength4,
    ChangeMaster,
    OldPassword,
    NewPassword,
    NewPasswordConfirm,
    MasterChanged,
    Attachments,
    AddAttachment,
    SaveAttachment,
    NoAttachments,
    // lab-hub
    Install,
    Update,
    Open,
    StartMenu,
    Desktop,
    Installed,
    Available,
    Downloading,
    Refresh,
    UpToDate,
    NotInstalled,
    // lab-hub onda 6
    Uninstall,
    Clean,
    OpenFolder,
    UninstallAsk,
    // lab-hub: card do próprio hub
    Running,
    Restart,
    // lab-clip: preferências (atalho/autostart/bandeja)
    Settings,
    Hotkey,
    Define,
    Autostart,
    PressKeys,
    CloseToTray,
    // lab-keys onda 17 — spinner dos jobs pesados
    Working,
    Unlocking,
    Importing,
    ReadingFile,
    ChangingMaster,
    AttachmentTooBig,
    // lab-keys onda 23 — URL/notas no form, export, favorito, lixeira, reveal
    Url,
    Notes,
    Favorite,
    Reveal,
    Export,
    ExportJson,
    ExportCsv,
    ExportWarning,
    Exported,
    Trash,
    TrashEmpty,
    Restore,
    Restored,
    DeleteForever,
    Deleted,
    Close,
    // lab-keys: pastas + filtro por anexos
    Folders,
    Folder,
    NewFolder,
    RenameFolder,
    DeleteFolder,
    NoFolder,
    FolderName,
    FilterAttachments,
    DeleteAttachment,
    AllItems,
    // lab-player: verificação de atualização (o prompt é o dialog do craft_update)
    CheckUpdates,
    // lab-player
    DragMediaHere,
    AddFile,
    AddFolder,
    Rotate,
    Mute,
    Unmute,
    // player: controles da fase 2 (motor de áudio)
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    Volume,
    /// Player: a fase 2 toca só áudio (o vídeo tem área própria na fase 3).
    AudioOnlyHint,
    /// Falhas do motor (a mensagem da UI é só a classe; detalhe vai pro log).
    ErrorNoOutputDevice,
    ErrorOpenFailed,
    ErrorDecodeFailed,
    ErrorSeekUnsupported,
    /// Vídeo com dimensões acima do limite (8K) — erro claro em vez de OOM.
    ErrorVideoTooLarge,
    /// O motor de áudio parou (falha interna fora dos catch_unwind).
    ErrorEngineStopped,
    /// Fullscreen do player (F) e a saída dele.
    Fullscreen,
    ExitFullscreen,
    /// Legendas externas (SRT/VTT): toggle e atraso.
    Subtitles,
    SubtitleDelay,
    /// Legenda embutida no container: faixa ativa e a tecla C (ciclo).
    SubtitleTrack,
    SubtitleEmbedded,
    /// Retomada explícita da posição salva (o play abre sempre do começo).
    ResumePosition,
    /// Tooltip do botão acima, com o tempo salvo (`{time}`).
    ResumeAt,
}

/// Todas as chaves — usado pelo teste de completude (fora de teste o match
/// exaustivo do `t()` já cobre; o const fica silenciado pra não warnar na
/// build da lib).
#[cfg_attr(not(test), allow(dead_code))]
const KEYS: &[Key] = &[
    Key::Language,
    Key::Theme,
    Key::Error,
    Key::Search,
    Key::Copy,
    Key::Cpu,
    Key::Memory,
    Key::Cores,
    Key::Uptime,
    Key::Disks,
    Key::Processes,
    Key::Filter,
    Key::Name,
    Key::End,
    Key::KillTitle,
    Key::KillAsk,
    Key::Confirm,
    Key::Cancel,
    Key::Clear,
    Key::ShowHide,
    Key::Quit,
    Key::Pin,
    Key::Unpin,
    Key::Delete,
    Key::Empty,
    Key::Vault,
    Key::MasterPassword,
    Key::Unlock,
    Key::NewVault,
    Key::WrongPassword,
    Key::Add,
    Key::Save,
    Key::Items,
    Key::Username,
    Key::Password,
    Key::Lock,
    Key::Totp,
    Key::QuickUnlock,
    Key::ForgetKey,
    Key::Edit,
    Key::SecondsShort,
    Key::TrashHint,
    Key::Import,
    Key::ImportKdbx,
    Key::ImportBitwarden,
    Key::ImportCsv,
    Key::ImportFile,
    Key::ImportPassword,
    Key::Imported,
    Key::SkippedCardIdentity,
    Key::ImportReportTitle,
    Key::Generator,
    Key::Generate,
    Key::Use,
    Key::Passphrase,
    Key::Words,
    Key::Separator,
    Key::Capitalize,
    Key::IncludeNumber,
    Key::Length,
    Key::Lowercase,
    Key::Uppercase,
    Key::Digits,
    Key::Symbols,
    Key::Strength0,
    Key::Strength1,
    Key::Strength2,
    Key::Strength3,
    Key::Strength4,
    Key::ChangeMaster,
    Key::OldPassword,
    Key::NewPassword,
    Key::NewPasswordConfirm,
    Key::MasterChanged,
    Key::Attachments,
    Key::AddAttachment,
    Key::SaveAttachment,
    Key::NoAttachments,
    Key::Install,
    Key::Update,
    Key::Open,
    Key::StartMenu,
    Key::Desktop,
    Key::Installed,
    Key::Available,
    Key::Downloading,
    Key::Refresh,
    Key::UpToDate,
    Key::NotInstalled,
    Key::Uninstall,
    Key::Clean,
    Key::OpenFolder,
    Key::UninstallAsk,
    Key::Running,
    Key::Restart,
    Key::Settings,
    Key::Hotkey,
    Key::Define,
    Key::Autostart,
    Key::PressKeys,
    Key::CloseToTray,
    Key::Working,
    Key::Unlocking,
    Key::Importing,
    Key::ReadingFile,
    Key::ChangingMaster,
    Key::AttachmentTooBig,
    Key::Url,
    Key::Notes,
    Key::Favorite,
    Key::Reveal,
    Key::Export,
    Key::ExportJson,
    Key::ExportCsv,
    Key::ExportWarning,
    Key::Exported,
    Key::Trash,
    Key::TrashEmpty,
    Key::Restore,
    Key::Restored,
    Key::DeleteForever,
    Key::Deleted,
    Key::Close,
    Key::Folders,
    Key::Folder,
    Key::NewFolder,
    Key::RenameFolder,
    Key::DeleteFolder,
    Key::NoFolder,
    Key::FolderName,
    Key::FilterAttachments,
    Key::DeleteAttachment,
    Key::AllItems,
    Key::CheckUpdates,
    Key::DragMediaHere,
    Key::AddFile,
    Key::AddFolder,
    Key::Rotate,
    Key::Mute,
    Key::Unmute,
    Key::Play,
    Key::Pause,
    Key::Stop,
    Key::Next,
    Key::Previous,
    Key::Volume,
    Key::AudioOnlyHint,
    Key::ErrorNoOutputDevice,
    Key::ErrorOpenFailed,
    Key::ErrorDecodeFailed,
    Key::ErrorSeekUnsupported,
    Key::ErrorVideoTooLarge,
    Key::ErrorEngineStopped,
    Key::Fullscreen,
    Key::ExitFullscreen,
    Key::Subtitles,
    Key::SubtitleDelay,
    Key::SubtitleTrack,
    Key::SubtitleEmbedded,
    Key::ResumePosition,
    Key::ResumeAt,
];

pub fn t(lang: Lang, key: Key) -> &'static str {
    let code = match lang {
        Lang::Pt => "pt-br",
        Lang::En => "en",
        Lang::Es => "es",
    };
    // A fonte inglesa é a chave de lookup do catálogo (padrão craft-i18n).
    let source = match key {
        Key::Language => "Language",
        Key::Theme => "Theme",
        Key::Error => "Error",
        Key::Search => "Search…",
        Key::Copy => "Copy",
        Key::Memory => "Memory",
        Key::Cores => "Cores",
        Key::Uptime => "Uptime",
        Key::Disks => "Disks",
        Key::Processes => "Processes",
        Key::Filter => "Filter",
        Key::Name => "Name",
        Key::End => "End task",
        Key::KillTitle => "End process?",
        Key::KillAsk => "Unsaved work in that app is lost.",
        Key::Confirm => "Confirm",
        Key::Cancel => "Cancel",
        Key::Clear => "Clear",
        Key::ShowHide => "Show/Hide",
        Key::Quit => "Quit",
        Key::Pin => "Pin",
        Key::Unpin => "Unpin",
        Key::Delete => "Delete",
        Key::Empty => "empty — copy something",
        Key::Vault => "Vault",
        Key::MasterPassword => "Master password",
        Key::Unlock => "Unlock",
        Key::NewVault => "New vault",
        Key::WrongPassword => "wrong password or corrupted/tampered file",
        Key::Add => "Add",
        Key::Save => "Save",
        Key::Items => "Items",
        Key::Username => "Username",
        Key::Password => "Password",
        Key::Lock => "Lock",
        Key::QuickUnlock => "Quick unlock on this PC",
        Key::ForgetKey => "Forget key",
        Key::Edit => "Edit",
        Key::Install => "Install",
        Key::Update => "Update",
        Key::Open => "Open",
        Key::StartMenu => "Start menu",
        Key::Desktop => "Desktop",
        Key::Installed => "installed",
        Key::Available => "available",
        Key::Downloading => "downloading",
        Key::Refresh => "Refresh",
        Key::UpToDate => "up to date",
        Key::NotInstalled => "not installed",
        Key::Uninstall => "Uninstall",
        Key::Clean => "Cleanup",
        Key::OpenFolder => "Open folder",
        Key::UninstallAsk => "Uninstall and remove shortcuts?",
        Key::Running => "running",
        Key::Restart => "restart Lab Hub",
        Key::TrashHint => "The item goes to trash — recoverable in the official LocalKeys.",
        Key::Import => "Import",
        Key::ImportBitwarden => "Bitwarden (encrypted export)",
        Key::ImportFile => "File",
        Key::ImportPassword => "File password",
        Key::Imported => "items imported",
        Key::SkippedCardIdentity => "card/identity skipped — the lab only manages login/note",
        Key::ImportReportTitle => "Import",
        Key::Generator => "Generator",
        Key::Generate => "Generate",
        Key::Use => "Use",
        Key::Passphrase => "Passphrase",
        Key::Words => "Words",
        Key::Separator => "Separator",
        Key::Capitalize => "Capitalize",
        Key::IncludeNumber => "Include number",
        Key::Length => "Length",
        Key::Lowercase => "Lowercase",
        Key::Uppercase => "Uppercase",
        Key::Digits => "Digits",
        Key::Symbols => "Symbols",
        Key::Strength0 => "terrible",
        Key::Strength1 => "weak",
        Key::Strength2 => "fair",
        Key::Strength3 => "strong",
        Key::Strength4 => "excellent",
        Key::ChangeMaster => "Change master password",
        Key::OldPassword => "Current password",
        Key::NewPassword => "New password",
        Key::NewPasswordConfirm => "Confirm new password",
        Key::MasterChanged => "master password changed",
        Key::Attachments => "Attachments",
        Key::AddAttachment => "Add attachment",
        Key::SaveAttachment => "Save",
        Key::NoAttachments => "no attachments",
        Key::Settings => "Settings",
        Key::Hotkey => "Global hotkey",
        Key::Define => "Set…",
        Key::Autostart => "Start with system",
        Key::PressKeys => "press the keys…",
        Key::CloseToTray => "Closing minimizes to tray.",
        Key::Working => "working…",
        Key::Unlocking => "unlocking…",
        Key::Importing => "importing…",
        Key::ReadingFile => "reading file…",
        Key::ChangingMaster => "changing the master password…",
        Key::AttachmentTooBig => "attachment too large (max 10 MiB — the whole vault is rewritten on every save)",
        Key::Url => "URL",
        Key::Notes => "Notes",
        Key::Favorite => "Favorite",
        Key::Reveal => "Show/hide",
        Key::Export => "Export",
        Key::ExportJson => "JSON (vault in the clear)",
        Key::ExportCsv => "CSV (spreadsheet)",
        Key::ExportWarning => "WARNING: the export is UNENCRYPTED — any app can read it. Use it only to migrate, then delete the file.",
        Key::Exported => "exported",
        Key::Trash => "Trash",
        Key::TrashEmpty => "trash is empty",
        Key::Restore => "Restore",
        Key::Restored => "restored",
        Key::DeleteForever => "Delete forever",
        Key::Deleted => "deleted forever",
        Key::Close => "Close",
        Key::CheckUpdates => "Check for updates",
        Key::Folders => "Folders",
        Key::Folder => "Folder",
        Key::NewFolder => "New folder",
        Key::RenameFolder => "Rename folder",
        Key::DeleteFolder => "Delete folder",
        Key::NoFolder => "No folder",
        Key::FolderName => "Folder name",
        Key::FilterAttachments => "Attachments",
        Key::DeleteAttachment => "Delete attachment",
        Key::AllItems => "All",
        Key::DragMediaHere => "Drag media here or pick from the playlist",
        Key::AddFile => "Add file",
        Key::AddFolder => "Add folder",
        Key::Rotate => "Rotate",
        Key::Mute => "Mute (M)",
        Key::Unmute => "Unmute (M)",
        Key::Play => "Play",
        Key::Pause => "Pause",
        Key::Stop => "Stop",
        Key::Next => "Next",
        Key::Previous => "Previous",
        Key::Volume => "Volume",
        Key::AudioOnlyHint => "Audio only",
        Key::ErrorNoOutputDevice => "No audio output device",
        Key::ErrorOpenFailed => "Could not open this file",
        Key::ErrorDecodeFailed => "Decoding failed",
        Key::ErrorSeekUnsupported => "This file cannot be seeked",
        Key::ErrorVideoTooLarge => "This video is too large to play (over 8K)",
        Key::ErrorEngineStopped => "The audio engine stopped",
        Key::Fullscreen => "Fullscreen (F)",
        Key::ExitFullscreen => "Exit fullscreen (F)",
        Key::Subtitles => "Subtitles (S)",
        Key::SubtitleDelay => "Subtitle delay",
        Key::SubtitleTrack => "Subtitle track (C)",
        Key::SubtitleEmbedded => "embedded",
        Key::ResumePosition => "Resume from last position",
        Key::ResumeAt => "Resume from {time}",
        Key::Cpu => "CPU",
        Key::Totp => "TOTP",
        Key::SecondsShort => "s",
        Key::ImportKdbx => "KeePass (.kdbx)",
        Key::ImportCsv => "CSV (Chrome/Edge/LastPass/1Password)",
    };
    let craft_lang = I18N.from_code(code).unwrap_or_else(|| I18N.from_code("en").expect("en é builtin"));
    I18N.t(craft_lang, source)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Equivalente do "tsc força completude": toda chave tem tradução nos
    /// 3 idiomas (match exaustivo + não-vazio).
    #[test]
    fn todas_as_chaves_tem_traducao() {
        for lang in Lang::ALL {
            for key in KEYS {
                let s = t(lang, *key);
                assert!(!s.is_empty(), "chave {key:?} vazia em {lang:?}");
            }
        }
    }

    /// As chaves da legenda embutida existem nos DOIS catálogos (faltando, `t()`
    /// cai no inglês em silêncio e o teste de completude não veria).
    #[test]
    fn catalogo_tem_as_chaves_de_legenda_embutida() {
        assert_eq!(t(Lang::Pt, Key::SubtitleTrack), "Faixa de legenda (C)");
        assert_eq!(t(Lang::Pt, Key::SubtitleEmbedded), "embutida");
        assert_eq!(t(Lang::Es, Key::SubtitleTrack), "Pista de subtítulos (C)");
        assert_eq!(t(Lang::Es, Key::SubtitleEmbedded), "integrada");
    }

    /// O motor craft-i18n está ligado: pt-br sai do catálogo TSV, não do inglês.
    #[test]
    fn craft_i18n_busca_no_catalogo() {
        assert_eq!(t(Lang::Pt, Key::Mute), "Mutar (M)");
        assert_eq!(t(Lang::Es, Key::Unmute), "Activar sonido (M)");
        assert_eq!(t(Lang::En, Key::Mute), "Mute (M)");
    }

    /// As chaves da retomada existem nos DOIS catálogos (faltando, `t()` cai no
    /// inglês em silêncio e o teste de completude não veria).
    #[test]
    fn catalogo_tem_as_chaves_de_retomada() {
        assert_eq!(t(Lang::Pt, Key::ResumePosition), "Retomar de onde parou");
        assert_eq!(t(Lang::Pt, Key::ResumeAt), "Retomar de {time}");
        assert_eq!(t(Lang::Es, Key::ResumePosition), "Reanudar desde donde quedó");
        assert_eq!(t(Lang::Es, Key::ResumeAt), "Reanudar desde {time}");
    }
}
