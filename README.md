# Beam

Cliente RDP (Remote Desktop Protocol) do ecossistema **Lyra OS**, para conexão com
máquinas Windows (desktops e servidores). Funciona em qualquer distribuição Linux moderna, com
integração visual e funcional prioritária com o Lyra (GNOME/Wayland).

- Protocolo RDP em Rust puro via [IronRDP](https://github.com/Devolutions/IronRDP) — sem FFI para
  libfreerdp.
- Interface em GTK4 + libadwaita.
- Credenciais protegidas no Serviço de Segredos do sistema (GNOME Keyring / KWallet) via
  [`oo7`](https://crates.io/crates/oo7) — nenhuma senha é gravada em disco em texto plano.
- Validação de certificado por confiança no primeiro uso (TOFU), como o `known_hosts` do SSH.

## Estrutura do repositório

- `beam-core`: motor de sessão RDP, sem dependência de nenhum toolkit gráfico.
- `beam-gtk`: frontend GTK4/libadwaita (binário `beam`).
- `data`: `.desktop`, metadados AppStream e ícones.

## Compilando

Dependências de sistema (nomes Fedora/openSUSE): `gtk4-devel`, `libadwaita-devel`, um compilador
Rust estável recente (`cargo`, `rustc`).

```sh
cargo build --release
./target/release/beam
```

Variável de ambiente `BEAM_LOG` controla o nível de log (`tracing-subscriber`), por exemplo
`BEAM_LOG=debug ./target/release/beam`. Senhas e conteúdo de clipboard nunca são registrados nos
logs.

## Uso

- **Ctrl+Alt+F12** libera a captura de teclado/mouse e sai da tela cheia — use se algum atalho
  local precisar funcionar durante uma sessão.
- Clicar de volta na área da sessão recaptura o teclado/mouse.
- Ctrl+Alt+Del pode ser enviado pelo botão no cabeçalho da janela de sessão.

## Limitações conhecidas (v1)

- Sem RemoteApp, RD Gateway, redirecionamento de drives/áudio/impressoras/smartcards/USB.
- Sem multi-monitor e sem redimensionamento dinâmico da resolução durante a sessão.
- O cursor remoto não é desenhado; a sessão sempre usa o cursor local do sistema.
- Clipboard: apenas texto (CF_UNICODETEXT), sem arquivos.
- Apenas RDP — sem VNC/SSH.

Essas limitações são decisões deliberadas de escopo para a v1, não bugs.

## Sobre a compressão volumétrica (bulk compression) desativada

O Beam **não** anuncia suporte a `INFO_COMPRESSION` (MPPC/K64) no `ClientInfo` PDU. Isso é
proposital: o pipeline de atualizações *slow-path* do IronRDP (`ironrdp-session::x224`) ainda não
decodifica dados volumetricamente comprimidos nesse caminho — só o *fast-path* tem essa
descompressão ligada. Servidores que respeitam o pedido do cliente (a compressão é opt-in) nunca
tentam comprimir dados slow-path, evitando o bug por completo; o fast-path continua usando
MPPC/K64 normalmente e não é afetado. É uma troca deliberada: um pouco menos de eficiência de
banda em favor de correção. Confirmado na prática — sem essa troca, uma conexão a um xrdp local
caía de forma consistente assim que o servidor mandava a primeira atualização de tela pelo
caminho slow-path com dados comprimidos.

## Licença

GPL-3.0-or-later. Veja [`LICENSE`](LICENSE).

### Responsividade da sessão

Escritas RDP têm prazo de 20 segundos; a reativação inteira usa um único prazo
de 20 segundos, inclusive quando o servidor envia apenas parte de um pacote.
Desconectar ou fechar a janela cancela imediatamente o trabalho pendente,
incluindo chaveiro, prompts e transporte. Uma escrita parcial encerra a conexão,
sem tentar reenviar o mesmo quadro nela.

Movimentos do ponteiro guardam somente a posição mais recente. Teclas, botões e
Ctrl+Alt+Del preservam a ordem em uma fila separada de 4096 comandos, sem bloquear
o GTK. Se essa fila esgotar, a sessão encerra com diagnóstico explícito: ela não
continua após perder uma transição de tecla. A notificação final de desconexão
independe da fila de eventos da interface e é entregue uma única vez.

Os testes do core exercitam o loop RDP real usando um transporte em memória com
capacidade de um byte: servidor que não lê, escrita parcial, reativação incompleta,
cancelamento e descarte da interface. O CI também abre e fecha uma janela GTK em
um display privado enquanto acumula entradas de teclado.
