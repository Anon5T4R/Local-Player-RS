# Experimento: TLS 100% Rust no update (Local Player RS)

> **Decisão do João (2026-11-08):** o provider `rustls-rustcrypto` é o caminho **único**; sem
> fallback por padrão ("até melhor não ter pra ver de verdade"). A válvula
> `LOCALPLAYER_TLS_FALLBACK=1` existe só como emergência. Não é padrão da suíte: vale **só para
> este app** e não deve ser copiado para o `craft-libs` de outros apps.

## O que muda no vendored

Tudo em `libs/craft-libs/crates/update` (o vendor é por app; os outros apps não são tocados).
Marcadores `// local-player-rs` nos pontos de divergência.

| Arquivo | Mudança |
| --- | --- |
| `Cargo.toml` | `rustls` direto (só pelo tipo `CryptoProvider`), `rustls-rustcrypto = 0.0.2-alpha` e `rustls-platform-verifier` (o `probe_pure` do teste faz o handshake sem o ureq) |
| `src/tls.rs` **(novo)** | estado do provider (`pure`/`fallback`), `pure_provider()`, `may_fallback`/`note_fallback` (só com a válvula), `status_line()` e o diagnóstico `probe_pure(host)` |
| `src/http.rs` | `Http` guarda dois conjuntos de agentes (`pure` e `standard`, mesmos timeouts/proxy); `with_agents` roda a requisição no provider ativo e só repete no padrão com a válvula ligada |
| `src/lib.rs` | `pub mod tls;` |

O resto do fluxo (release feed, download com resume/checksum, proxy, platform verifier) fica
idêntico: muda só o `CryptoProvider` que o ureq recebe
(`TlsConfig::unversioned_rustls_crypto_provider`).

## Comportamento

| Situação | O que acontece |
| --- | --- |
| Padrão | provider **pure** (`rustls-rustcrypto`). Log de sessão: `tls_provider=pure` |
| Handshake pure falha (TLS/transporte) | o erro sobe normal: linha de status do "Verificar atualizações" + `event=update_check_failed detail=...` no log; **nenhum retry** |
| `LOCALPLAYER_TLS_FALLBACK=1` | a MESMA requisição é repetida uma vez com o provider padrão (ring) e o log registra `tls_provider=fallback(motivo)` |
| Resposta HTTP (404/500) | não é falha de handshake: nunca repete |

`tls_provider=pure` sai pendente e é gravado antes da primeira linha da sessão (execução sem
playback nem check não cria log; um check manual abre com `event=update_check start`). Um fallback
abre o log na hora.

## Teste real (feito)

```text
cargo test --test tls_update_check -- --ignored --nocapture
tls_provider (antes) = tls_provider=pure
github negociou = TLSv1_3 TLS13_AES_128_GCM_SHA256
release encontrada = v0.3.0 (4)
tls_provider (depois) = tls_provider=pure
```

O teste (1) faz um handshake direto com `api.github.com` usando o provider puro e imprime
protocolo/suite, e (2) roda o caminho real do `craft-update` (`Http` + `check_for_update`) contra
`Anon5T4R/Local-Player-RS`. Precisa de rede; por isso é `#[ignore]` (não roda no CI).

## `cargo tree -i ring`

O ring **continua no grafo** (antes e depois): `ureq` compila `_ring` e o `rustls` depende dele. Ele
só deixou de ser o provider das requisições do update. Para tirá-lo do grafo seria preciso abrir
mão da válvula (ou trocá-la por `native-tls`/SChannel) — decisão em aberto.

## `cargo deny`

O provider puxa `rustls-webpki 0.102.8` (só pelo tipo `alg_id`; a verificação de cadeia é do
`rustls-webpki 0.103.16` + verificador do SO) e `rsa 0.9`/`crypto-bigint 0.5` (verificação de
assinatura pública; o cliente não decifra nem assina). Sete advisories foram adicionadas ao
`ignore` do `deny.toml` com essa justificativa. **Revisitar antes de release**: ou o upstream migra
para o webpki 0.103, ou o experimento não vai para release.

## Como reverter

1. remover as três deps do `Cargo.toml` do `craft-update` e `pub mod tls;`;
2. `Http::new` volta a montar um conjunto só de agentes (o construtor `agents(...)` pode ficar);
3. apagar `src/tls.rs`, o `with_agents`, a linha `tls_provider` do `session_log` e o teste
   `tests/tls_update_check.rs`;
4. `cargo update -p rustls-rustcrypto` não é suficiente: os ignores do `deny.toml` também saem
   junto.
