# Experimento: TLS 100% Rust no update (Local Player RS)

> **Decisão do João (2026-11-08):** o provider `rustls-rustcrypto` é o caminho **único**, sem
> fallback e sem provider C no binário ("até melhor não ter pra ver de verdade"). Não é padrão da
> suíte: vale **só para este app** e não deve ser copiado para o `craft-libs` de outros apps.
>
> Segunda decisão (mesma data, fechando o experimento): **resolver em vez de esperar upstream** —
> o provider virou fork vendored (`libs/rustls-rustcrypto`), o `rustls-webpki 0.102` saiu e o
> `ring` saiu do grafo de vez.

## O que mudou no vendored

Tudo em `libs/craft-libs/crates/update` (o vendor é por app; os outros apps não são tocados).
Marcadores `// local-player-rs` nos pontos de divergência.

| Arquivo | Mudança |
| --- | --- |
| `Cargo.toml` | `rustls` direto (só pelo tipo `CryptoProvider`), `rustls-rustcrypto` (resolvido pelo `[patch.crates-io]` para o fork) e `rustls-platform-verifier` (o `probe_pure` do teste faz o handshake sem o ureq). `ureq` **sem** a feature `_ring` |
| `src/tls.rs` **(novo)** | `pure_provider()`, `status_line()` (sempre `tls_provider=pure`) e o diagnóstico `probe_pure(host)` |
| `src/http.rs` | `Http` guarda **um** conjunto de agentes (`request(...)`); o `TlsConfig` sempre carrega o provider puro |
| `src/lib.rs` | `pub mod tls;` |

O resto do fluxo (release feed, download com resume/checksum, proxy, platform verifier) fica
idêntico: muda só o `CryptoProvider` que o ureq recebe
(`TlsConfig::unversioned_rustls_crypto_provider`).

## O fork (`libs/rustls-rustcrypto`)

Fonte do crates.io (`rustls-rustcrypto 0.0.2-alpha`, MIT OR Apache-2.0), vendored para que a
correção não espere release do upstream. Duas divergências, ambas com comentário
`// local-player-rs:`:

1. **sai a dependência `rustls-webpki 0.102`.** O upstream só a usava pelas constantes `alg_id`
   (o identificador DER do algoritmo de assinatura). Essas constantes existem no
   `rustls-pki-types` desde a 1.14 e têm **exatamente os mesmos bytes** (os `data/alg-*.der` dos
   dois crates são idênticos) — então a verificação não muda, só some um webpki velho do grafo.
   Era a origem de 5 advisories ignoradas no `deny.toml`.
2. `[workspace]` no manifesto: o fork é workspace próprio, fora do workspace do app
   (`libs/` está no `exclude`).

Nada mais foi tocado — o resto do código (e a formatação) é o upstream. Para sincronizar uma
release nova, refaça só esses dois pontos (README do fork tem o resumo).

## Comportamento

| Situação | O que acontece |
| --- | --- |
| Toda requisição do update | provider **puro** (`rustls-rustcrypto`). Log de sessão: `tls_provider=pure` |
| Handshake falha (TLS/transporte) | o erro sobe normal: linha de status do "Verificar atualizações" + `event=update_check_failed detail=...` no log; **nenhum retry** |
| Resposta HTTP (404/500) | resposta, não handshake: erro normal, nunca repete |

Não existe mais `LOCALPLAYER_TLS_FALLBACK`: o caminho com `ring` não é compilado, então a válvula
não teria como funcionar. `tls_provider=pure` sai pendente e é gravado antes da primeira linha da
sessão (execução sem playback nem check não cria log; um check manual abre com
`event=update_check start`).

## Teste real (feito)

```text
cargo test --test tls_update_check -- --ignored --nocapture
tls_provider (antes) = tls_provider=pure
github negociou = TLSv1_3 TLS13_AES_128_GCM_SHA256
release encontrada = v0.4.0 (4)
tls_provider (depois) = tls_provider=pure
```

O teste (1) faz um handshake direto com `api.github.com` usando o provider puro e imprime
protocolo/suite, e (2) roda o caminho real do `craft-update` (`Http` + `check_for_update`) contra
`Anon5T4R/Local-Player-RS`. Precisa de rede; por isso é `#[ignore]` (não roda no CI).

## `cargo tree -i ring`

**Fora do grafo** (idem `aws-lc-sys`):

```text
$ cargo tree -i ring
warning: nothing to print.
$ cargo tree -i aws-lc-sys
error: package ID specification `aws-lc-sys` did not match any packages
```

De onde o `ring` saía (e por que não sai mais):

- `ureq` com a feature `_ring` ligava `rustls/ring` → removida; o ureq agora usa
  `rustls-no-provider` e recebe o provider pelo `TlsConfig` (o `Http` monta os agentes com
  `unversioned_rustls_crypto_provider(tls::pure_provider())`, então o ureq nunca procura default).
- `rustls-webpki 0.102` (via provider) puxava `ring` → saiu junto com a dependência no fork.
- `rustls-platform-verifier` com `default-features = false` só depende de `rustls/std`, sem
  provider; o `Verifier::new(provider)` recebe o nosso.

`rustls-webpki` no grafo é **só** a 0.103.16 (do `rustls` 0.23.46) — a verificação de cadeia é ela
+ o verificador do SO.

## `cargo deny`

Os 5 ignores do `rustls-webpki 0.102` foram **removidos** (RUSTSEC-2026-0049/0098/0099/0104/0382).
Sobram dois, sem correção possível hoje, com justificativa revisada no `deny.toml`:

| Advisory | Por que fica |
| --- | --- |
| `RUSTSEC-2023-0071` (`rsa`, Marvin) | `patched = []` no próprio advisory: nem 0.9.10 nem 0.10.0-rc têm correção. O cliente só **verifica** assinatura com chave pública (CertificateVerify do servidor); o ataque é de recuperação de chave privada por timing. Sair do `rsa` = não verificar servidor com certificado RSA |
| `RUSTSEC-2026-0387` (`crypto-bigint 0.5.5`) | corrigido em `>= 0.7.6`, mas o 0.5 chega pelo `rsa 0.9`, que exige essa série; migrar exigiria `rsa 0.10.0-rc` (API nova) e não tiraria a advisory do `rsa` de cima. As operações secret-dependentes são de chave privada; o cliente só faz aritmética de chave pública |

`cargo deny check`: `advisories ok, bans ok, licenses ok, sources ok`.

## Como reverter (se o experimento morrer)

1. remover `[patch.crates-io] rustls-rustcrypto` do `Cargo.toml` do app e as três deps do
   `Cargo.toml` do `craft-update` + o `pub mod tls;`;
2. apagar `libs/rustls-rustcrypto/`, `src/tls.rs`, o teste `tests/tls_update_check.rs` e o campo
   `session.tls(...)`/`status_line` do `src/main.rs`;
3. `Http::new` volta a um `TlsConfig` sem provider explícito e o `ureq` recupera a feature `_ring`
   (ou `rustls` default) — aí o `ring` volta ao grafo;
4. os dois ignores do `deny.toml` também saem junto (voltam a valer para `_ring`/`rsa` só se
   ficarem no grafo).
