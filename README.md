# tslib

Une bibliothèque modulaire et cross-platform pour créer des clients TeamSpeak 3 et 6 et des bots.

## 📦 Modules

| Module | Description |
|--------|-------------|
| `tslib-core` | Cœur de la bibliothèque : connexion, identité, événements |
| `tslib-audio` | Capture/lecture audio, codecs Opus |
| `tslib-chat` | Messages texte, parsing BBCode |
| `tslib-channel` | Gestion des canaux, arborescence |
| `tslib-bot` | Framework pour créer des bots avec commandes |
| `tslib-ffi` | Bindings C pour intégration Python/Java/Go |

## 🚀 Démarrage rapide

### Installation

Ajoutez à votre `Cargo.toml` :

```toml
[dependencies]
tslib-core = { path = "path/to/tslib/crates/tslib-core" }
tslib-bot = { path = "path/to/tslib/crates/tslib-bot" }  # Pour les bots
```

### Exemple : Bot simple

```rust
use tslib_bot::{Bot, BotConfig};
use tslib_core::Identity;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let identity = Identity::create()?;

    let config = BotConfig::builder()
        .address("localhost:9987")
        .identity(identity)
        .nickname("MonBot")
        .command_prefix("!")
        .build()?;

    let mut bot = Bot::new(config).await?;

    bot.command("ping", |ctx| async move {
        ctx.reply("Pong!").await
    }).await;

    bot.run().await
}
```

### Exemple : Client voix

```rust
use tslib_core::{Client, ClientConfig, Identity};
use tslib_audio::{AudioManager, AudioConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let identity = Identity::create()?;

    let config = ClientConfig::builder()
        .address("localhost:9987")
        .identity(identity)
        .nickname("MonClient")
        .build()?;

    let client = Client::connect(config).await?;

    let audio = AudioManager::new(AudioConfig::default())?;
    audio.start_capture().await?;
    audio.start_playback().await?;

    // Boucle d'événements...

    client.disconnect().await
}
```

## 🛠️ Compilation

### Prérequis

- Rust 1.75+
- Cargo
- Pour l'audio : ALSA (Linux), CoreAudio (macOS), WASAPI (Windows)

### Build

```bash
# Clone le repo
git clone https://github.com/your/tslib.git
cd tslib

# Build tout le workspace
cargo build --release

# Build un exemple
cargo build --release -p simple-bot
```

### Exécuter les exemples

```bash
# Bot simple
cargo run -p simple-bot -- --server localhost --nickname MyBot

# Client voix
cargo run -p voice-client -- --server localhost --nickname MyClient
```

## 📚 Documentation

```bash
cargo doc --workspace --open
```

## 🔧 FFI (Bindings C)

Pour utiliser tslib depuis d'autres langages :

```bash
# Build la bibliothèque partagée
cargo build --release -p tslib-ffi

# Le header C est généré dans crates/tslib-ffi/include/tslib.h
```

### Exemple C

```c
#include "tslib.h"

int main() {
    tslib_init();

    TsLibIdentity* identity = tslib_identity_create();
    TsLibClient* client = tslib_client_connect(
        "localhost:9987",
        identity,
        "MyBot",
        NULL  // pas de mot de passe
    );

    if (client) {
        tslib_client_send_server_message(client, "Hello from C!");
        tslib_client_free(client);
    }

    tslib_identity_free(identity);
    tslib_shutdown();
    return 0;
}
```

### Python (via ctypes)

```python
import ctypes

lib = ctypes.CDLL("./target/release/libtslib_ffi.so")

lib.tslib_init()
identity = lib.tslib_identity_create()
client = lib.tslib_client_connect(
    b"localhost:9987",
    identity,
    b"PythonBot",
    None
)
# ...
```

## 📋 Fonctionnalités

### Implémentées

- [x] Structure du projet modulaire
- [x] Interface de connexion
- [x] Gestion des identités
- [x] Système d'événements
- [x] Framework bot avec commandes
- [x] Parsing BBCode
- [x] Gestion des canaux
- [x] Bindings FFI

### En cours

- [ ] Intégration complète avec tsclientlib
- [ ] Audio capture/playback réel (cpal)
- [ ] Encodage/décodage Opus
- [ ] Tests d'intégration
- [ ] Bindings Python (PyO3)
- [ ] Bindings Java (JNI)

## 🏗️ Architecture

```
tslib/
├── crates/
│   ├── tslib-core/       # Cœur : connexion, événements, état
│   ├── tslib-audio/      # Audio : capture, playback, Opus
│   ├── tslib-chat/       # Chat : messages, BBCode
│   ├── tslib-channel/    # Canaux : arborescence, gestion
│   ├── tslib-bot/        # Bot : commandes, plugins
│   └── tslib-ffi/        # FFI : bindings C
├── examples/
│   ├── simple-bot/       # Exemple de bot
│   └── voice-client/     # Exemple de client voix
└── Cargo.toml            # Workspace
```

## 📄 Licence

MIT OR Apache-2.0

## 🙏 Crédits

- [tsclientlib](https://github.com/ReSpeak/tsclientlib) - Implémentation du protocole TeamSpeak
- [opus](https://opus-codec.org/) - Codec audio
- [cpal](https://github.com/RustAudio/cpal) - Audio cross-platform
