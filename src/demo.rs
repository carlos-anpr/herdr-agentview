//! Datos de ejemplo para `--demo`: enseñan la interfaz sin herdr y sin datos reales.

use crate::app::{App, Msg};
use crate::config::Config;
use crate::model::*;
use crate::util::now;
use std::path::PathBuf;
use std::sync::Arc;

#[allow(clippy::too_many_arguments)] // Fixture: columnas explícitas para cada panel de demostración.
fn pane(id: &str, ws: (&str, &str, u32), label: Option<&str>, kind: Kind, raw: Option<&str>, status: Status, title: &str, cwd: &str) -> Pane {
    Pane {
        pane_id: id.into(),
        tab_id: format!("{}:t1", ws.0),
        workspace_id: ws.0.into(),
        workspace_label: ws.1.into(),
        workspace_number: ws.2,
        tab_label: "1".into(),
        label: label.map(String::from),
        kind,
        agent_raw: raw.map(String::from),
        status,
        title: title.into(),
        cwd: PathBuf::from(cwd),
        session: Some(SessionRef::Id(format!("demo-{id}"))),
        session_agent: raw.map(String::from),
        focused: false,
        revision: 1,
    }
}

fn ev(ago: i64, kind: EventKind, title: &str) -> Event {
    Event { ts: now() - ago, kind, title: title.into(), detail: None, state: EventState::Ok, duration_ms: None, child: None }
}

fn ms(mut e: Event, ms: u64) -> Event {
    e.duration_ms = Some(ms);
    e
}

fn detail(mut e: Event, d: &str) -> Event {
    e.detail = Some(d.into());
    e
}

fn state(mut e: Event, s: EventState) -> Event {
    e.state = s;
    e
}

fn todo(text: &str, state: TodoState) -> Todo {
    Todo { text: text.into(), state }
}

fn file(path: &str, edits: u32, ago: i64) -> FileTouch {
    FileTouch { path: path.into(), edits, last_ts: now() - ago }
}

pub fn app(cfg: Config) -> App {
    use EventKind::*;
    let mut app = App::offline(cfg);
    app.origin_ws = Some("w1".into());
    app.origin_pane = Some("w1:p1".into());
    let shop = ("w1", "tienda-online", 1);
    let land = ("w2", "landing", 2);
    let mut docs = pane("w1:p5", shop, Some("docs"), Kind::Shell, None, Status::Unknown, "zsh", "/home/ana/tienda-online");
    docs.session_agent = Some("claude".into());
    let panes = vec![
        pane("w1:p1", shop, Some("lider"), Kind::Claude, Some("claude"), Status::Working, "Pasarela de pagos", "/home/ana/tienda-online"),
        pane("w1:p2", shop, Some("backend"), Kind::Codex, Some("codex"), Status::Working, "codex", "/home/ana/tienda-online/api"),
        pane("w1:p3", shop, Some("frontend"), Kind::OpenCode, Some("opencode"), Status::Blocked, "OC | Formulario de pago", "/home/ana/tienda-online/web"),
        pane("w1:p4", shop, Some("pruebas"), Kind::Omp, Some("omp"), Status::Done, "π tests e2e", "/home/ana/tienda-online"),
        docs,
        pane("w2:p1", land, Some("copy"), Kind::Claude, Some("claude"), Status::Idle, "Textos de la portada", "/home/ana/landing"),
        pane("w2:p2", land, Some("revisor"), Kind::Codex, Some("codex"), Status::Idle, "codex", "/home/ana/landing"),
    ];
    app.on_msg(Msg::Snapshot(Ok(Snapshot { panes, focused_pane_id: Some("w1:p1".into()), focused_workspace_id: Some("w1".into()) })));

    let t = now();
    let lider = SessionInfo {
        title: Some("Pasarela de pagos".into()),
        model: Some("claude-opus-5-5".into()),
        effort: Some("high".into()),
        last_prompt: Some("Integra la pasarela de pagos: reparte el trabajo y coordina que todo quede probado".into()),
        last_reply: Some(
            "Reparto hecho. **backend** implementa el webhook y la firma; **frontend** el formulario con validación; \
             **pruebas** ya ha pasado la batería e2e contra el sandbox.\n\nMientras tanto reviso con dos subagentes \
             cómo guardamos hoy los pedidos y qué exige la normativa sobre los datos de tarjeta."
                .into(),
        ),
        started_at: Some(t - 2400),
        updated_at: Some(t - 3),
        turn_started_at: Some(t - 760),
        tokens_in: 1_840_000,
        tokens_out: 38_200,
        context_used: Some(142_000),
        context_window: Some(1_000_000),
        cost_usd: Some(4.31),
        limits: vec![],
        todos: vec![
            todo("Leer el contrato de la pasarela", TodoState::Done),
            todo("Repartir backend, frontend y pruebas", TodoState::Done),
            todo("Revisar el modelo de pedidos", TodoState::Active),
            todo("Integrar las tres ramas", TodoState::Pending),
            todo("Prueba completa en sandbox", TodoState::Pending),
        ],
        events: vec![
            ev(760, Prompt, "Integra la pasarela de pagos: reparte el trabajo y coordina que todo quede probado"),
            ms(ev(752, Read, "docs/pasarela.md"), 40),
            ms(ev(748, Search, "\"createOrder\" en api/src"), 310),
            ms(ev(731, Shell, "Ver ramas y estado del repo"), 1200),
            ev(700, Todo, "Repartir backend, frontend y pruebas"),
            ms(ev(690, Tool, "herdr · delegate backend"), 900),
            ms(ev(684, Tool, "herdr · delegate frontend"), 850),
            ms(ev(679, Tool, "herdr · delegate pruebas"), 870),
            ev(640, Reply, "Reparto hecho. backend implementa el webhook y la firma; frontend el formulario…"),
            state(ev(210, Task, "Mapear cómo se guardan los pedidos"), EventState::Running),
            state(ev(208, Task, "Qué exige PCI para datos de tarjeta"), EventState::Running),
            ms(detail(ev(95, Edit, "docs/plan-pagos.md"), "+48 −2"), 60),
            state(ev(12, Shell, "Esperar a que backend termine los tests"), EventState::Running),
        ],
        children: vec![
            Child {
                id: "a1".into(),
                name: "Mapear cómo se guardan los pedidos".into(),
                agent_type: Some("Explore".into()),
                model: Some("claude-haiku-5-5".into()),
                running: true,
                started_at: Some(t - 210),
                updated_at: Some(t - 4),
                tokens_in: 212_000,
                tokens_out: 3_100,
                events: vec![
                    ev(210, Prompt, "Localiza dónde se crean y guardan los pedidos y resume el esquema"),
                    ms(ev(204, Search, "\"orders\" en api/"), 280),
                    ms(ev(198, Read, "api/src/orders/model.ts"), 30),
                    ms(ev(190, Read, "api/src/orders/service.ts"), 35),
                    ms(ev(170, Search, "\"INSERT INTO orders\""), 240),
                    ms(ev(120, Read, "api/migrations/0042_orders.sql"), 28),
                    state(ev(6, Read, "api/src/payments/legacy.ts"), EventState::Running),
                ],
                last_reply: None,
            },
            Child {
                id: "a2".into(),
                name: "Qué exige PCI para datos de tarjeta".into(),
                agent_type: Some("general-purpose".into()),
                model: Some("claude-sonnet-5-5".into()),
                running: false,
                started_at: Some(t - 208),
                updated_at: Some(t - 60),
                tokens_in: 96_000,
                tokens_out: 2_400,
                events: vec![
                    ev(208, Prompt, "Resume qué podemos guardar y qué no de una tarjeta según PCI DSS"),
                    ms(ev(200, Web, "PCI DSS almacenamiento de PAN y CVV"), 2100),
                    ms(ev(150, Web, "https://docs.pasarela.example/tokenizacion"), 1800),
                    ev(60, Reply, "No se guarda nunca el CVV. El PAN solo tokenizado por la pasarela…"),
                ],
                last_reply: Some(
                    "## Qué se puede guardar\n- **Nunca** el CVV, ni cifrado.\n- El PAN solo como *token* que devuelve la pasarela.\n- Los 4 últimos dígitos y la marca sí, para enseñarlos en el pedido.\n\n## Qué implica aquí\n- El formulario debe enviar la tarjeta directamente a la pasarela (campos alojados).\n- En `orders` basta con `payment_token`, `card_last4` y `card_brand`."
                        .into(),
                ),
            },
        ],
        files: vec![file("docs/plan-pagos.md", 3, 95)],
    };
    let backend = SessionInfo {
        model: Some("gpt-6.1-sol".into()),
        effort: Some("medium".into()),
        last_prompt: Some("Implementa el webhook de la pasarela con verificación de firma y sus tests".into()),
        last_reply: Some("Webhook listo en `api/src/payments/webhook.ts`. Falta que pasen los tests de firma con el secreto rotado.".into()),
        started_at: Some(t - 700),
        updated_at: Some(t - 2),
        turn_started_at: Some(t - 688),
        tokens_in: 912_000,
        tokens_out: 21_400,
        context_used: Some(171_000),
        context_window: Some(475_000),
        limits: vec![Limit { name: "5h".into(), used_pct: 23.0, resets_at: Some(t + 9000) }, Limit { name: "7d".into(), used_pct: 41.0, resets_at: Some(t + 300_000) }],
        todos: vec![
            todo("Endpoint POST /payments/webhook", TodoState::Done),
            todo("Verificar la firma HMAC", TodoState::Done),
            todo("Idempotencia por id de evento", TodoState::Active),
            todo("Tests de integración", TodoState::Pending),
        ],
        events: vec![
            ev(688, Prompt, "Implementa el webhook de la pasarela con verificación de firma y sus tests"),
            ms(ev(680, Read, "docs/pasarela.md"), 35),
            ms(ev(671, Search, "\"router.post\" en api/src"), 190),
            ms(detail(ev(610, Edit, "api/src/payments/webhook.ts"), "+86"), 70),
            ms(detail(ev(540, Edit, "api/src/payments/signature.ts"), "+41"), 55),
            ms(detail(ev(470, Shell, "npm test -- payments"), "npm test -- payments"), 18_400),
            state(ms(detail(ev(430, Shell, "npm test -- signature"), "npm test -- signature"), 9_300), EventState::Error),
            ms(detail(ev(380, Edit, "api/src/payments/signature.ts"), "+6 −3"), 50),
            ms(detail(ev(300, Shell, "npm test -- signature"), "npm test -- signature"), 8_900),
            ev(240, Todo, "Idempotencia por id de evento"),
            ms(detail(ev(180, Edit, "api/src/payments/webhook.ts"), "+22 −4"), 64),
            ms(detail(ev(90, Edit, "api/test/webhook.test.ts"), "+118"), 80),
            state(detail(ev(34, Shell, "npm test -- webhook"), "npm test -- webhook --runInBand"), EventState::Running),
        ],
        files: vec![file("api/src/payments/webhook.ts", 2, 180), file("api/src/payments/signature.ts", 2, 380), file("api/test/webhook.test.ts", 1, 90)],
        ..Default::default()
    };
    let frontend = SessionInfo {
        title: Some("Formulario de pago".into()),
        model: Some("opencode-go/deepseek-v4.1-flash".into()),
        effort: Some("max".into()),
        last_prompt: Some("Monta el formulario de pago con los campos alojados de la pasarela".into()),
        last_reply: Some("Necesito instalar `@pasarela/hosted-fields` para seguir.".into()),
        started_at: Some(t - 690),
        updated_at: Some(t - 190),
        turn_started_at: Some(t - 684),
        tokens_in: 402_000,
        tokens_out: 9_800,
        context_used: Some(64_000),
        context_window: Some(256_000),
        cost_usd: Some(0.28),
        events: vec![
            ev(684, Prompt, "Monta el formulario de pago con los campos alojados de la pasarela"),
            ms(ev(670, Read, "web/src/checkout/Checkout.tsx"), 30),
            ms(detail(ev(600, Edit, "web/src/checkout/PaymentForm.tsx"), "+132"), 90),
            ev(200, Reply, "Necesito instalar @pasarela/hosted-fields para seguir."),
            state(detail(ev(190, Shell, "npm install @pasarela/hosted-fields"), "npm install @pasarela/hosted-fields"), EventState::Running),
        ],
        files: vec![file("web/src/checkout/PaymentForm.tsx", 1, 600)],
        ..Default::default()
    };
    let pruebas = SessionInfo {
        title: Some("tests e2e".into()),
        model: Some("opencode-go/deepseek-v4.1-flash".into()),
        last_prompt: Some("Pasa la batería e2e de checkout contra el sandbox y dime qué falla".into()),
        last_reply: Some("**18 de 18 en verde** contra el sandbox. El flujo de reembolso tarda 9 s: lo dejo anotado en `docs/e2e.md`.".into()),
        started_at: Some(t - 680),
        updated_at: Some(t - 130),
        turn_started_at: Some(t - 679),
        tokens_in: 233_000,
        tokens_out: 5_100,
        cost_usd: Some(0.09),
        events: vec![
            ev(679, Prompt, "Pasa la batería e2e de checkout contra el sandbox y dime qué falla"),
            ms(detail(ev(660, Shell, "Lanzar la batería e2e"), "npx playwright test checkout --reporter=line"), 412_000),
            ms(detail(ev(150, Edit, "docs/e2e.md"), "+9"), 40),
            ev(130, Reply, "18 de 18 en verde contra el sandbox. El flujo de reembolso tarda 9 s…"),
        ],
        files: vec![file("docs/e2e.md", 1, 150)],
        ..Default::default()
    };
    let copy = SessionInfo {
        model: Some("claude-sonnet-5-5".into()),
        last_prompt: Some("Propón tres titulares para la portada".into()),
        last_reply: Some("Tres propuestas, de más sobria a más atrevida…".into()),
        updated_at: Some(t - 5400),
        turn_started_at: Some(t - 5460),
        events: vec![ev(5460, Prompt, "Propón tres titulares para la portada"), ev(5400, Reply, "Tres propuestas, de más sobria a más atrevida…")],
        ..Default::default()
    };
    let revisor = SessionInfo {
        model: Some("gpt-6.1-sol".into()),
        last_prompt: Some("Revisa la accesibilidad de la portada".into()),
        updated_at: Some(t - 9000),
        events: vec![ev(9100, Prompt, "Revisa la accesibilidad de la portada"), ev(9000, Reply, "Dos contrastes por debajo de AA y un botón sin etiqueta.")],
        last_reply: Some("Dos contrastes por debajo de AA y un botón sin etiqueta.".into()),
        ..Default::default()
    };
    for (id, info) in [("w1:p1", lider), ("w1:p2", backend), ("w1:p3", frontend), ("w1:p4", pruebas), ("w2:p1", copy), ("w2:p2", revisor)] {
        app.on_msg(Msg::Session { pane: id.into(), info: Some(Arc::new(info)) });
    }
    for (id, text) in [
        ("w1:p1", "\x1b[38;2;217;119;87m●\x1b[0m Reparto hecho. \x1b[1mbackend\x1b[0m implementa el webhook y la firma;\n  \x1b[1mfrontend\x1b[0m el formulario; \x1b[1mpruebas\x1b[0m ya ha pasado la batería e2e.\n\n\x1b[38;2;217;119;87m●\x1b[0m \x1b[1mAgent\x1b[0m(Mapear cómo se guardan los pedidos)\n  \x1b[2m⎿ Read api/src/payments/legacy.ts\x1b[0m\n\x1b[38;2;217;119;87m●\x1b[0m \x1b[1mAgent\x1b[0m(Qué exige PCI para datos de tarjeta)\n  \x1b[2m⎿ Done (3 tool uses · 98k tokens)\x1b[0m\n\n\x1b[38;2;217;119;87m✻\x1b[0m Esperando a backend… \x1b[2m(12m 40s · esc to interrupt)\x1b[0m\n\n\x1b[2m────────────────────────────────────────────────────\x1b[0m\n❯ \n\x1b[2m────────────────────────────────────────────────────\x1b[0m\n  \x1b[38;2;255;107;128m⏵⏵ bypass permissions on\x1b[0m"),
        ("w1:p2", "\x1b[2m•\x1b[0m Edited \x1b[36mapi/test/webhook.test.ts\x1b[0m (\x1b[32m+118\x1b[0m)\n\n\x1b[2m•\x1b[0m Running \x1b[1mnpm test -- webhook --runInBand\x1b[0m\n  \x1b[2m└\x1b[0m PASS test/signature.test.ts\n    \x1b[32m✓\x1b[0m rechaza una firma caducada (41 ms)\n    \x1b[32m✓\x1b[0m acepta el secreto rotado (12 ms)\n    RUNS test/webhook.test.ts\n\n\x1b[2m• Working (34s • esc to interrupt)\x1b[0m\n\n\x1b[48;2;40;44;52m› \x1b[2mAsk Codex to do anything\x1b[0m\n\n  \x1b[2mGPT-6.1-Sol medium · Context 36% used · 5h 77% left\x1b[0m"),
        ("w1:p3", "  \x1b[1mnpm install @pasarela/hosted-fields\x1b[0m\n\n  \x1b[48;2;60;45;20m                                              \x1b[0m\n  \x1b[48;2;60;45;20m \x1b[33m△ Permission required\x1b[39m                        \x1b[0m\n  \x1b[48;2;60;45;20m   Ejecutar: npm install @pasarela/hosted-…   \x1b[0m\n  \x1b[48;2;60;45;20m                                              \x1b[0m\n  \x1b[48;2;60;45;20m   \x1b[7m Allow once \x1b[27m   Allow always   Reject      \x1b[0m\n  \x1b[48;2;60;45;20m                                              \x1b[0m\n\n  \x1b[2mBuild · DeepSeek V4.1 Flash · max\x1b[0m"),
        ("w1:p4", " \x1b[32m✔\x1b[0m 18 passed \x1b[2m(6.9m)\x1b[0m\n\n \x1b[1m18 de 18 en verde\x1b[0m contra el sandbox. El flujo de\n reembolso tarda 9 s: lo dejo anotado en docs/e2e.md.\n\n                                   \x1b[2mTODO 4/4 · ☑ done\x1b[0m\n \x1b[35mπ\x1b[0m > DeepSeek V4.1 Flash > ~/tienda-online > $0.09"),
        ("w1:p5", "\x1b[32mana@portatil\x1b[0m \x1b[34m~/tienda-online\x1b[0m $ "),
    ] {
        app.on_msg(Msg::Screen { pane: id.into(), lines: Arc::new(crate::ansi::parse(text)) });
    }
    app
}
