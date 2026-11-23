# telegram-cybersec-bot

Bot de Telegram profesional, escrito en **Rust**, que agrega noticias de ciberseguridad procedentes de fuentes públicas (RSS de medios reconocidos + búsqueda en GitHub) y entrega digests categorizados a los suscriptores **cada 12 horas** o **bajo demanda**.

- 🔁 Programación automática con `tokio` (cada 12 h por defecto, configurable).
- 📡 Agregación concurrente de feeds RSS/Atom con `feed-rs` + `reqwest`.
- 🐙 Búsqueda de POCs/exploits en GitHub (`/search/repositories`).
- 🧠 Categorización por palabras clave: **Ataques · Vulnerabilidades · POCs · IOCs**.
- 💾 Persistencia simple en JSON (suscriptores + deduplicación de noticias).
- 🛡️ Manejo de errores con `anyhow`/`thiserror`, logs estructurados con `tracing`.
- 🐳 Dockerfile multi-stage con runtime mínimo (Debian slim + tini).

---

## 📑 Índice

1. [Crear el bot en BotFather](#1-crear-el-bot-en-botfather)
2. [Requisitos previos](#2-requisitos-previos)
3. [Instalación y configuración](#3-instalación-y-configuración)
4. [Compilar y ejecutar](#4-compilar-y-ejecutar)
5. [Despliegue con Docker](#5-despliegue-con-docker-opcional)
6. [Comandos disponibles](#6-comandos-disponibles)
7. [Fuentes utilizadas](#7-fuentes-utilizadas)
8. [Arquitectura del proyecto](#8-arquitectura-del-proyecto)
9. [Configuración avanzada](#9-configuración-avanzada)
10. [Tests](#10-tests)
11. [Solución de problemas](#11-solución-de-problemas)
12. [Licencia](#12-licencia)

---

## 1. Crear el bot en BotFather

1. Abre Telegram y busca al usuario **[@BotFather](https://t.me/BotFather)**.
2. Inicia una conversación y envía el comando `/newbot`.
3. BotFather te pedirá:
   - Un **nombre para mostrar** (ej.: *Mi Bot de Ciberseguridad*).
   - Un **username único** que debe terminar en `bot` (ej.: `mi_cybersec_bot`).
4. Cuando termines, BotFather te enviará el **token de la API**, algo así como:
   ```
   123456789:AAH...etc...
   ```
   ⚠️ Trátalo como una contraseña: cualquiera con ese token puede controlar tu bot.
5. *(Opcional)* Configura iconos y descripción con `/setuserpic`, `/setdescription`, `/setabouttext`.
6. *(Opcional)* Activa los comandos en el menú: `/setcommands` y pega:
   ```
   start - Mostrar bienvenida y ayuda
   latest - Obtener resumen bajo demanda
   subscribe - Activar notificaciones automáticas
   unsubscribe - Desactivar notificaciones automáticas
   status - Ver estado de tu suscripción
   help - Mostrar ayuda
   ```
   *(El propio bot publica esta lista automáticamente al arrancar, pero hacerlo manualmente acelera la primera visualización.)*

---

## 2. Requisitos previos

| Herramienta | Versión mínima | Comprobación |
| --- | --- | --- |
| **Rust** (toolchain `stable`) | 1.75+ | `rustc --version` |
| **Cargo** | 1.75+ | `cargo --version` |
| **OpenSSL / libssl** | No es necesaria (usamos `rustls`) | — |
| **Docker** *(opcional)* | 20.10+ | `docker --version` |

### Instalar Rust (Linux/macOS)

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

### Instalar Rust (Windows)

Descarga [`rustup-init.exe`](https://rustup.rs/) y ejecútalo. Tras la instalación reinicia la terminal.

---

## 3. Instalación y configuración

### 3.1 Clonar el repositorio

```bash
git clone <url-del-repo>.git telegram-cybersec-bot
cd telegram-cybersec-bot
```

### 3.2 Crear el archivo `.env`

```bash
cp .env.example .env
```

Edita `.env` y pon tu token:

```dotenv
TELEGRAM_BOT_TOKEN=123456789:AAH...  # de BotFather
```

Las demás variables son opcionales y vienen con valores razonables.

---

## 4. Compilar y ejecutar

### 4.1 Modo debug (rápido para desarrollo)

```bash
cargo run
```

### 4.2 Modo release (recomendado en producción)

```bash
cargo build --release
./target/release/telegram-cybersec-bot
```

### 4.3 Verificar que arranca correctamente

Los primeros logs deberían parecerse a:

```
 INFO Starting telegram-cybersec-bot version="0.1.0"
 INFO Configuration loaded interval_secs=43200 state_path="state.json" github_auth=false
 INFO State file not found, creating new one. path="state.json"
 INFO Scheduler started interval_secs=43200
```

Abre Telegram, busca a tu bot por su `@username` y envía `/start`. Debería responderte con el mensaje de bienvenida.

---

## 5. Despliegue con Docker (opcional)

### 5.1 Construir la imagen

```bash
docker build -t telegram-cybersec-bot:latest .
```

### 5.2 Ejecutar

```bash
docker run -d \
  --name cybersec-bot \
  --restart=unless-stopped \
  -e TELEGRAM_BOT_TOKEN="123456789:AAH..." \
  -v "$(pwd)/data:/home/botuser/data" \
  telegram-cybersec-bot:latest
```

El volumen `./data` guarda `state.json` (suscriptores + deduplicación) y sobrevive a los reinicios.

### 5.3 Logs

```bash
docker logs -f cybersec-bot
```

---

## 6. Comandos disponibles

| Comando | Acción |
| --- | --- |
| `/start` | Mensaje de bienvenida y lista de comandos. |
| `/help` | Igual que `/start`. |
| `/latest` | Genera un resumen **bajo demanda** (sin afectar al ciclo automático). |
| `/subscribe` | Activa la entrega automática del digest cada 12 h. |
| `/unsubscribe` | Detiene la entrega automática. |
| `/status` | Muestra si estás suscrito y el total de suscriptores. |

---

## 7. Fuentes utilizadas

### Feeds RSS / Atom
- The Hacker News
- Bleeping Computer
- SecurityWeek
- Krebs on Security
- CISA / US-CERT Cybersecurity Advisories
- NVD — Recent CVEs
- NVD — Recent Analysis

### GitHub
Búsquedas a `https://api.github.com/search/repositories` filtradas por *repos creados en los últimos 7 días* con las palabras clave **POC**, **exploit** y **CVE**.

> Puedes añadir, quitar o modificar fuentes editando:
> - `src/aggregator/rss_feeds.rs` → constante `DEFAULT_FEEDS`
> - `src/aggregator/github_search.rs` → constante `QUERIES`

### Categorías (palabras clave)

| Categoría | Detectada por |
| --- | --- |
| 🚨 **Ataques** | *attack*, *breach*, *hacked*, *ransomware*, *phishing*, *DDoS*, *campaign*, *actively exploited*… |
| 🛡️ **Vulnerabilidades** | IDs `CVE-AAAA-NNNNN`, *vulnerability*, *zero-day*, *RCE*, *patch tuesday*, *CVSS*… |
| 💥 **POCs** | *POC*, *proof of concept*, *exploit code*, *public exploit*, *weaponized*, *metasploit module*… |
| 🧬 **IOCs / Threat Intel** | *IOC*, *threat intelligence*, *YARA*, *C2 server*, *MITRE ATT&CK*, hashes (32–64 hex), IPs defangueadas… |

---

## 8. Arquitectura del proyecto

```
telegram-cybersec-bot/
├── Cargo.toml
├── Dockerfile
├── .env.example
├── README.md
└── src/
    ├── main.rs                 # Punto de entrada y wiring
    ├── config/mod.rs           # Carga de variables de entorno
    ├── models/mod.rs           # Tipos compartidos (NewsItem, Category…)
    ├── persistence/mod.rs      # State (JSON) — subscribers + dedup
    ├── aggregator/
    │   ├── mod.rs              # Façade que orquesta RSS + GitHub
    │   ├── rss_feeds.rs        # Cliente RSS/Atom concurrente
    │   ├── github_search.rs    # Búsqueda de POCs/exploits en GitHub
    │   └── categorizer.rs      # Reglas + regex para clasificar
    ├── bot/
    │   ├── mod.rs              # Dispatcher de teloxide
    │   ├── commands.rs         # Enum con los /comandos
    │   └── handlers.rs         # Lógica de cada comando + render del digest
    └── scheduler/mod.rs        # Bucle tokio cada N segundos
```

### Flujo de datos

```
                       ┌─────────────────────┐
   tokio::interval ──▶ │  scheduler::run     │
   /latest ─────────▶  │  bot::handlers      │
                       └────────┬────────────┘
                                ▼
                       ┌─────────────────────┐
                       │ aggregator::collect │
                       └───┬─────────────┬───┘
                           │             │
                           ▼             ▼
                    rss_feeds       github_search
                       │  │             │  │
                       └──┴──── merge ──┴──┘
                                ▼
                       categorizer::categorise_all
                                ▼
                       bot::handlers::render_digest
                                ▼
                          send a Telegram
```

---

## 9. Configuración avanzada

Variables del archivo `.env` (todos los valores tienen defaults razonables salvo `TELEGRAM_BOT_TOKEN`):

| Variable | Valor por defecto | Descripción |
| --- | --- | --- |
| `TELEGRAM_BOT_TOKEN` | *(obligatorio)* | Token emitido por BotFather. |
| `GITHUB_TOKEN` | *(vacío)* | PAT clásico de GitHub para subir el rate limit a 5000 req/h. |
| `SCHEDULE_INTERVAL_SECS` | `43200` (12 h) | Intervalo entre digests automáticos, en segundos. |
| `MAX_ITEMS_PER_CATEGORY` | `8` | Máximo de noticias por categoría en cada digest. |
| `STATE_PATH` | `state.json` | Ruta del fichero de estado. |
| `HTTP_TIMEOUT_SECS` | `20` | Timeout de cada petición HTTP saliente. |
| `USER_AGENT` | `telegram-cybersec-bot/<ver>` | User-Agent enviado por `reqwest`. |
| `RUST_LOG` | `info,…` | Filtro de logs estilo `tracing-subscriber`. |

---

## 10. Tests

Ejecuta la batería de tests unitarios:

```bash
cargo test
```

Incluye tests para:
- Carga/guardado del estado (`persistence`).
- Reglas de categorización (`aggregator::categorizer`).
- Limpieza de texto HTML (`aggregator::rss_feeds`).
- Renderizado del digest y escaping MarkdownV2 (`bot::handlers`).

---

## 11. Solución de problemas

| Síntoma | Causa probable / Solución |
| --- | --- |
| `Error: TELEGRAM_BOT_TOKEN is not set` | El archivo `.env` no existe o no está en el directorio desde el que ejecutas `cargo run`. Verifica con `ls -la .env`. |
| `error sending request for url ...` repetido | El feed o GitHub están caídos o tu red bloquea HTTPS. Los fallos individuales se loguean como `WARN` y no detienen el bot. |
| Telegram responde *“Bot was blocked by the user”* | Un suscriptor te bloqueó. El error queda en `WARN`; puedes eliminarlo manualmente de `state.json` o esperar a que use `/unsubscribe`. |
| `HTTP 403` de GitHub | Has agotado el rate limit anónimo (60 req/h). Define `GITHUB_TOKEN` con un PAT clásico (sin scopes). |
| El bot no responde | Asegúrate de que sólo hay **una instancia** activa con el mismo token: Telegram desconectará a la antigua con `409 Conflict`. |

---

## 12. Licencia

MIT — siéntete libre de adaptar el bot a tus necesidades. Las fuentes RSS y la API de GitHub mantienen sus propios términos de uso; respétalos.