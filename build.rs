//! Embute o ícone no .exe do local-player-rs (Windows) — sem isso o Explorer
//! mostra o ícone genérico de executável. Fora do Windows: no-op.

fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icon.ico");
        res.compile().expect("winresource");
    }
}
