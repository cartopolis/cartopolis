---
topic: component-library
triggers: ["component library", "componentbibliotheek", "jsx", "rsx", "view!", "replace egui", "egui vervangen", "egui weg", "Dioxus", "Leptos", "Yew", "Xilem", "Masonry", "Vello", "bevy_ui", "bevy_feathers", "bevy_ui_widgets", "bevy_flair", "signals", "reactive", "design tokens", "@garage44/mithril", "mithril", "components.css", "web_theme.rs", "theme/controls.rs", "ui-vocabulary", "one design", "same style", "dezelfde stijl", "toolkit"]
updated: 2026-11-23
---

# Eén componentbibliotheek voor het web en de Bevy-client

Een studie, geen code. Gevraagd 2026-11-23: "Onze mithril library is nu alleen
voor het web. Kunnen we het ombouwen naar een jsx-achtige library, waarbij we de
ui components ook kunnen gebruiken in bijv. Cartopolis? Dan kunnen we de wasm
build combineren met UI components voor web en dezelfde style behouden. Ter
inspiratie: Dioxus, Leptos, Yew. Doe eerst onderzoek naar het beste design voor
een component-based library. We willen uiteindelijk egui vervangen."

Drie bijlagen dragen de feiten: `egui-inventory.md` (alles wat de client nu
van egui krijgt, per bestand en regel), `web-inventory.md` (de fork, atlas-web,
midas en podium, met aantallen) en `landscape-2026-11.md` (Dioxus, Leptos,
Yew, Xilem/Vello, iced, GPUI, Slint en Bevy's eigen UI, met versies, data en
bronnen). Hier staat wat daaruit volgt. Het blad `v1-architectuur.png` tekent
het voorstel.

![architectuur](v1-architectuur.png)

## 0. Samenvatting

- **Niets in Rust tekent vandaag één component zowel in de DOM als op een
  GPU-canvas.** Xilem is de enige stack met beide backends en deelt alleen de
  reactieve laag, niet de widgets. Elke andere "web + native"-belofte is één
  renderer op een canvas — en dan verlies je in de browser de echte tekst, het
  toetsenbord op de telefoon, IME en de schermlezer. Wat we willen bestaat dus
  niet om te adopteren; het bestaat wel om te *bouwen*, en de bouwstenen zijn
  er sinds dit jaar.
- **Het ontwerp: één gesloten vocabulaire, één stijlbron, twee backends.** De
  componenten van de fork (`Button`, `FieldText`, `Card`, `Dialog`, `Shell`, …)
  worden in Rust één keer gedefinieerd tegen zes primitieven (`Box`, `Text`,
  `Image`, `Input`, `Scroll`, `Layer`) en een kleine stijl-subset. De DOM-backend
  schrijft elementen met de bestaande `m-`-klassen en de stijl-subset wordt als
  CSS *gegenereerd*; de Bevy-backend schrijft `Node`-entiteiten in `bevy_ui`.
  Fijnmazige signalen (het model dat de fork al heeft) drijven beide. Een
  `view!`-macro met JSX-syntaxis bouwt de bomen, zodat TSX regel voor regel
  overzet.
- **Het web-deel van de client wordt DOM, niet canvas.** Het wasm-wereldframe
  (sinds 2026-11-22 alleen nog de wereld) tekent zijn panelen dan met dezelfde
  CSS als cartopolis.org, over het canvas heen. Dat is precies de "wasm build
  combineren met de web-UI" uit de vraag, en het is ook de enige manier om op
  een telefoonbrowser een toetsenbord bij een tekstveld te krijgen: geen Rust-
  toolkit kan dat vanaf een canvas (`landscape-2026-11.md`, Bevy, iced, GPUI).
- **Niet adopteren: Dioxus+Blitz, Leptos, Xilem/Masonry, iced, GPUI.** Elk
  valt op één van drie dingen: geen tweede backend (Leptos, Yew, Sycamore,
  iced, GPUI), een zwaar en nog bèta-renderer in de client (Blitz: Servo's
  Stylo, MPL, ~12 MB, 18–24 ms frames gemeten, Bevy-koppeling alleen op
  Dioxus-alpha), of geen release in dertien maanden en een kapotte wasm-build
  (Masonry). Wat we wél overnemen: Bevy's eigen `bevy_ui` + `bevy_ui_widgets`
  als native backend, `bevy_flair` als voorbeeld van CSS-op-`Node`, en de
  renderer-agnostische template-gedachte van Dioxus voor de macro.
- **Eerst meten, dan bouwen.** Twee feiten beslissen of `bevy_ui` nu al kan:
  Bevy 0.20 verloor sub-pixelpositionering van tekst bij de overgang naar
  Parley (hersteld op `main` 2026-11-17 voor 0.21) en legt de hele UI-boom
  elke frame opnieuw uit (incrementeel in 0.21). Fase 0 fotografeert
  `bevy_ui`-tekst naast egui 0.36 op 412×900 / 1,5× en meet de bouwkosten van
  de features. Daarna fase 1: kern + macro + DOM-backend, bewezen op één
  atlas-webpagina die pixelgelijk uit Rust komt.
- **Omvang.** De client heeft 78.012 regels in `systems/gui`, 4.847
  `egui::`-verwijzingen in 118 bestanden en 41 schermen; atlas-web 15.894 regels
  TSX. egui vervangen is een programma van maanden, paneel voor paneel, waarbij
  egui en de nieuwe laag een tijd naast elkaar tekenen. Elke fase hieronder
  landt alleen en laat iets zien.

## 1. Wat er nu is: twee helften, met de hand gelijk gehouden

Het product tekent zijn UI twee keer. **cartopolis.org** is een Mithril-app in
TypeScript (`crates/atlas/web`, 15.894 regels TSX + 10.429 TS + 9.271 CSS) op
de fork `@garage44/mithril` (12.100 regels, waarvan 1.609 componenten en 2.232
CSS). Die fork heeft al wat de vraag noemt: signalen (`signal`, `computed`,
`effect`, `state()`), een `model: {value}`-conventie voor elk veld, 31
componenten, tokens als `light-dark()`-paren, SSR. midas' hub gebruikt dezelfde
fork (13.864 regels TSX); podium gebruikt hem niet.

**De app** tekent met egui 0.36 door een eigen `egui-wgpu`-brug. Wat de
inventaris telt: 56 paneelbestanden, 78.012 regels in `systems/gui`, nog eens
43 bestanden buiten `gui/` die egui aanspreken (de routekaart, de zoeker, de
bestuurder), 92 zwevende `Area`s, 216 `painter()`-aanroepen (kompas,
zoomrail, pins, gezichten, QR), 64 `TextEdit`s, een eigen schermtoetsenbord
van 1.289 regels (Android heeft geen IME in NativeActivity), een eigen
afwijsladder van 21 treden, en een MCP-boom die via AccessKit uit egui leest.

**De naad tussen beide is dun en handmatig.** `web_theme.rs` genereert de
kleurtokens en drie radii naar `theme.css` (een test bewaakt dat); afstanden,
controlhoogtes en elk component zelf zijn met de hand nagetekend
(`theme/controls.rs:1-34` is een tabel "web-component → egui-functie", met de
getallen 38/44 px, radius 8, padding 16/12). midas kopieert cartopolis'
`theme.css` met de hand en is in vier waarden afgedreven; cartopolis'
`theme::LIGHT` zegt op zijn beurt midas' tokens te hebben overgenomen. Op
2026-11-22 was de conclusie nog "één widgetset in `theme::controls`, dan
scherm voor scherm" (`ui-vocabulary.md`, "The controls are the web's"); deze
vraag zet een stap verder: niet nátekenen, maar één keer definiëren.

## 2. Wat "één bibliotheek" kan betekenen

Een component dat in een TypeScript-runtime draait, kan niet in een native
Rust-binary draaien, en omgekeerd niet zonder wasm. Dus zijn er drie vormen:

1. **TS blijft de bron, Rust tekent na** — de situatie van nu, met meer
   generatie (tokens, maten). Goedkoop, maar elk component blijft twee
   implementaties met twee gedragingen.
2. **Rust wordt de bron; het web krijgt hem als wasm.** Cartopolis.org, het
   wereldframe en de app delen één componentcode. midas' hub en andere
   TS-apps delen alleen nog de *CSS* (de gegenereerde `m-`-klassen en tokens),
   tot ze zelf overzetten — of nooit.
3. **TS wordt de bron en draait ook native** via een ingebedde JS-engine.
   Afgewezen: Bun draait niet op Android noch in wasm, en de client krijgt
   dan een tweede runtime en een tweede GC voor zijn UI.

De vraag kiest 2 ("de wasm build combineren met UI components voor web"). Wat
het web-deel betreft is dat haalbaar zonder cartopolis.org om te gooien: de
Rust-DOM-backend schrijft dezelfde klassen en tokens als de TS-componenten,
dus een Rust-`Button` en een Mithril-`Button` zijn in één pagina pixelgelijk,
en pagina's kunnen één voor één over.

## 3. Het landschap in één tabel

Kolommen zijn de eisen uit de vraag en uit de inventaris; de feiten met datum
en bron staan in `landscape-2026-11.md`.

| | DOM-backend | Bevy-backend | Eén componentcode | Fijnmazig reactief | Tekstinvoer web/Android | Toegankelijk | Stand 2026-11 | Licentie |
|---|---|---|---|---|---|---|---|---|
| **Dioxus + Blitz (bedina)** | ja, volwassen | voorbeeld van 19 bestanden op Dioxus 0.8-alpha / Blitz 0.3-bèta; Blitz tekent in een wgpu-textuur op Bevy's device | ja (echte CSS via Stylo) | per component, niet per node | web ja; Blitz-IME sinds nov 2026 | AccessKit, ARIA pas compleet nov 2026 | Blitz "bèta, veel bugs"; 18–24 ms frames gemeten; ~12 MB | MIT/Apache, **Stylo MPL** |
| **Leptos** | ja, beste wasm-maat | nee: renderer-generiek verwijderd vóór 0.7, DOM vast; alleen fork + cfg | nee | ja (`reactive_graph`) | web ja | web ja | 0.9-rc, één onderhouder | MIT |
| **Yew** | ja, VDOM | nee, geen renderer-trait | nee | nee | web ja | web ja | onderhoud, geen ontwikkeling | MIT/Apache |
| **Xilem / Masonry + Vello** | `xilem_web` (andere elementen) | `masonry_core` zonder winit/wgpu, host-vorm klopt; nooit elders gehost | nee: twee vocabulaires | per view-boom herbouwd | Parley `PlainEditor`, IME ja | AccessKit ja | geen release sinds 2025-10-29; `masonry_winit` bouwt niet voor wasm | Apache |
| **iced** | nee (canvas) | ja, `integration`-voorbeeld | nee | Elm, herbouw | geen Android, web-IME open | **geen** | 0.14, één reviewer | MIT |
| **GPUI** | nee (canvas, zonder tekst) | nee: bezit venster en swapchain | nee | hybride | geen mobiel | gemerged, uitgezet | gemeenschapswerk gestopt 2025-12 | Apache |
| **egui (nu)** | nee (canvas) | ja, eigen brug | nee | immediate | eigen toetsenbord | AccessKit alleen voor MCP | 0.36; hinting sinds 0.34 | MIT/Apache |
| **Eigen: `bevy_ui` + DOM** | te bouwen (web-sys) | `Node`-entiteiten, picking, `TextInput`, focus, a11y op desktop | **ja, per ontwerp** | te bouwen (klein, naar de fork) | DOM op web; `EditableText` + IME op Android | DOM op web, AccessKit native | 0.20 tekst zonder sub-pixel; 0.21 herstelt | eigen |

Twee observaties uit de tabel. Ten eerste: het "renderer-agnostische" zit bij
alle kandidaten in de reactieve laag en nooit in de widgets — ook Dioxus'
Blitz is "één CSS-engine op twee plekken", geen gedeelde widget. Ten tweede:
elke reactieve laag die op Bevy gebouwd is (tien bekeken, zes dood) eindigt
in hetzelfde: *signaal of diff → `Commands` op `Node`-entiteiten*. Dat is dus
de vorm die werkt; wat ontbreekt is er één die ook de DOM aanstuurt.

## 4. Het ontwerp

### 4.1 Een gesloten vocabulaire, geen HTML

Apps componeren uit de componenten van de fork en uit lay-outprimitieven,
nooit uit HTML-elementen. Dat is de hele truc: wat niet in de vocabulaire zit
kan niet platform-afhankelijk worden geschreven. React Native doet het zo,
Dioxus/Blitz juist niet (en betaalt met Stylo).

- **Primitieven (6):** `Box` (flex/grid-container met stijl), `Text` (runs
  met stijl, selectie), `Image` (bron, fit, UV-crop), `Input` (één- en
  meerregelig, masker, hint, `model`), `Scroll` (richting, kleef-onder,
  balk-stijl), `Layer` (zwevend: orde-klasse, anker aan een rect of een
  wereldpunt, modaal). Een zevende, `Canvas` (vectoropdrachten: pad, cirkel,
  lijn, mesh met UV), draagt wat nu 216 keer `painter()` is; op het web een
  `<canvas>`/SVG, in Bevy een `UiMaterial`-mesh.
- **Componenten:** de 31 van de fork, naam voor naam, met dezelfde props en
  dezelfde `model`-conventie (`FieldText { model, label, help, error,
  validation }`), plus wat de app nodig heeft en het web nog niet had:
  `Sheet` (drie rusthoogten, de vinger erin — `motion::grabber`), `Page`
  (de pagina-stapel met `page_turn`), `Segmented`, `MenuEntry`,
  `PanelHeader`, `Pin`.
- **Validatie en motion** komen mee: de fork's `validation` is al DOM-vrij; de
  app's `theme::motion` (vijf tijden, twee curves, reduced motion) wordt de
  motion-laag voor beide — de hub's `app.css` had al dezelfde getallen.

### 4.2 Eén stijlbron: tokens én componentstijlen in Rust, CSS gegenereerd

Vandaag genereert `web_theme.rs` alleen kleuren. In het ontwerp staan **alle**
tokens (kleur, afstand, radius, type, controlhoogte, schaduw, duur) als één
Rust-tabel, en elk component draagt zijn stijl in een kleine subset:
`display`, `gap`, `padding`, `margin`, `size/min/max`, `radius`, `border`,
`background`, `color`, `font`, `shadow`, `opacity`, `overflow`, `position`,
`transition` — met toestanden `hover`, `pressed`, `focus`, `disabled`,
`selected`. Daaruit komen:

- **`components.css`** voor het web, gegenereerd zoals `theme.css` nu
  (klassen `m-button[data-variant=primary]` enz.), met een test die de
  ingecheckte file bewaakt. De handgeschreven 2.232 regels CSS van de fork
  krimpen tot wat de subset niet uitdrukt.
- **`Node`/`BackgroundColor`/`BorderRadius`/`TextFont`** in Bevy, direct uit
  dezelfde waarden; `bevy_flair` laat zien dat zelfs echte CSS-selectors op
  `Node` haalbaar zijn, maar de subset heeft geen cascade nodig: een
  component kent zijn eigen stijl.

Zo is "dezelfde stijl" een eigenschap van de bouw, niet van discipline. Wat
de subset bewust níet doet: cascade, selectors over componentgrenzen,
`::before`, container queries. Een `compact`/`wide`-vertakking (nu
`compact()` 25×) wordt één `Breakpoint`-signaal dat beide backends vullen.

### 4.3 Signalen: fijnmazig, synchroon, één flush per frame

Het reactieve model is dat van de fork: `signal`, `computed`, `effect`,
`state()` met `$key`, en `model: {value}`. In Rust: een kleine eigen kern
(Sycamore/Floem-stijl: synchrone propagatie, `batch`, thread-lokaal), zodat
(a) de API dezelfde is als die het team al kent, (b) er geen async-executor of
`Send`-bound in de weg zit — onder wasm met atomics is elk DOM-handle `!Send`,
en Bevy's `World` ook — en (c) de kern zonder afhankelijkheden blijft, wat hem
later ook voor Pinnacle bruikbaar maakt (§4.6). Alternatief als de eigen kern
tegenvalt: `reactive_graph` (Leptos' kern, actief, `ImmediateEffect` +
`poll_local`), tegen de prijs van een globale executor.

Waarom fijnmazig en niet VDOM of immediate: de wereld verandert elke frame
(posities, tellers, de HUD). Met signalen schrijft één Bevy-systeem in
`ClientPhase::Input` de ECS-waarden in hun signalen, één `flush` propageert, en
alleen de gebonden bladeren (een tekstnode, een attribuut) worden geraakt. Een
VDOM diff't het hele subboom; egui doet de hele UI elke frame opnieuw en heeft
daarom de "stille-frame"-sloot (`egui-quiet-frame-staleness.md`) nodig. Een
`WorldAnchored`-laag wordt in dit model gewoon een `Layer` waarvan het anker
een signaal is.

### 4.4 De `view!`-macro: JSX-syntaxis, bouwer-aanroepen

```rust
view! {
    <Form on:submit={submit}>
        <Card title={title}>
            <Text class="hint">{hint}</Text>
            <FieldText model={s.username} label="Username" autocomplete="username"
                       disabled={busy} validation={v.username} />
            <FieldText model={s.password} label="Password" kind=Password
                       autocomplete="current-password" disabled={busy} validation={v.password} />
            <Button variant=Primary submit=true loading={busy} disabled={!v.valid()}>
                "Sign in"
            </Button>
        </Card>
    </Form>
}
```

Dat is `elements/signin/signin.tsx:46-70` van vandaag, bijna regel voor regel.
De macro (rstml, zoals Leptos) breidt uit naar bouwer-aanroepen op de
componenttypes (`Card::new().title(..).child(..)`), zodat zonder macro
hetzelfde kan en rust-analyzer de props kent. Hoekhaken, niet Dioxus' accolades,
omdat 15.894 regels TSX overzetten het eerste grote karwei is. Dynamiek:
`{expr}` als tekst of kind, `if`/`match` als kind, `for x in xs key=..`,
`on:click`, `class:active={sig}`, `model=` tweezijdig. Dioxus' les nemen we
mee: de macro mag later een statisch sjabloon met dynamische gaten uitstoten
(goedkoper voor rustc en voor de Bevy-backend), zonder dat de syntaxis
verandert.

### 4.5 Twee backends achter één `Renderer`-trait

Een `Renderer` doet wat tachys' trait deed (knopen maken, tekst zetten,
attribuut zetten, invoegen, verwijderen) **plus** wat die miste en waarom
niemand er ooit een lay-out-renderer achter kreeg: `measure`/`layout` is de
backend's zaak, stijl komt als waarden, invoer komt als genormaliseerde
events (`Pointer`, `Key`, `Text`, `Ime`, `Scroll`), focus en IME-rect gaan
terug.

- **DOM** (wasm32, en als string voor SSR): `web-sys`; primitieven →
  elementen met de gegenereerde klassen; `Input` → `<input>`/`<textarea>` (dus
  browser-IME, toetsenbord, autocorrectie, wachtwoordbeheer — alles wat een
  canvas niet krijgt); `Layer` → `position: fixed` met `z-index` per
  orde-klasse; `Dialog` → `<dialog>`; `Select` → `<select>` (de fork's keuze,
  om de telefoon-picker). SSR levert dezelfde HTML als string voor de
  CMS-pagina's, die nu via Bun in de simulator renderen (`pages/render.rs`).
- **Bevy** (`bevy_ui`, `bevy_text`, `bevy_ui_render`, `bevy_picking`,
  `bevy_input_focus`, allemaal nu uit): primitieven → `Node`-entiteiten in
  één `UiTargetCamera`; tekst → `Text`/`TextSpan` (Parley, hinting aan);
  `Input` → `bevy_ui_widgets::TextInput` over `EditableText` (selectie,
  klembord, IME-preedit; placeholder, undo en masker zijn in 0.20 nog niet
  geïmplementeerd — die bouwt de component zelf, zoals nu); events ←
  picking-observers (`On<PointerClick>`, drag, scroll, `PointerId::Touch`),
  dus geen geëmuleerde muis meer; focus ← `InputFocus`; a11y ← `bevy_a11y`
  (desktop; Android alleen met GameActivity). Wat `bevy_ui` 0.20 níet heeft
  en de backend zelf doet: sleep-scrollen met traagheid (issue #18923 open),
  de overgangen (`motion` schrijft `Node`-waarden; `TryStableInterpolate` op
  `Val` bestaat), zwevende lagen met orde-klassen (`GlobalZIndex`), en de
  wereld-ankers (projectie → `UiTransform` per frame).
- **De "panel chain" blijft, in een andere vorm.** egui's volgorde-regel
  bestond omdat de hit-test de tekenvolgorde volgde. In de DOM en in
  `bevy_ui` beslist de boom + z-index; de nieuwe laag claimt de aanraking
  eerst (`owns_click` wordt "landde dit op een UI-knoop") en de kaart krijgt
  de rest. De afwijsladder (`dismiss.rs`) blijft één enum; elk `Layer`,
  `Page` en `Sheet` registreert zich erin.

### 4.6 Het wereldframe tekent DOM; later Pinnacle

Op het web is de client sinds 2026-11-22 alleen de wereld, in een iframe van
cartopolis.org. Zijn resterende panelen (zoeker, lagen, Places, routekaart,
instellingen, kaartjes, pins — `egui-inventory.md` §5) tekent de DOM-backend
*boven* het canvas, binnen het frame, met de `components.css` van de site. Dat
geeft het frame echte tekstvelden en dezelfde stijl als de pagina eromheen,
en maakt de egui-wasm-omweg (non-send manager, eigen toetsenbord op de
telefoon-tier, dpr-foefjes) overbodig. Pins en bubbels krijgen per frame hun
`transform` uit `world_to_viewport`, zoals kaartbibliotheken dat doen.

Pinnacle heeft eigen `ui`- en `layout`-crates (`no_std`, geen externe
crates). Een kern zonder afhankelijkheden en een `Renderer`-trait maken een
derde backend op Pinnacle's `gfx`/`layout` mogelijk; dat is een optie, geen
onderdeel van dit voorstel.

## 5. Wat de Bevy-backend van egui moet overnemen, en wat het oplost

De inventaris (`egui-inventory.md` §3) is de checklist. De grote posten en
hoe het ontwerp ze beantwoordt:

| Vandaag in egui | In het ontwerp |
|---|---|
| 64 `TextEdit`s + eigen toetsenbord 1.289 regels + aanraakhandvatten | `Input` → DOM `<input>` op web (browser doet alles); `TextInput`/`EditableText` native, schermtoetsenbord blijft als component voor Android (NativeActivity heeft geen IME; Feathers heeft er zelf een) |
| 92 `Area`s met `Order`, `move_to_top`-sorteerval | `Layer` met orde-klassen → `GlobalZIndex` / `z-index`; geen stabiele-sort-truc |
| 216 `painter()`-sites, 159 `Shape`s, 9 meshes | `Canvas`-primitief; de brand-mark, kompas, pins, QR, gezichten, gradiënt-meshes; tijdens de migratie mag egui's painter blijven onder de nieuwe laag |
| 33 `TextureHandle`s (foto's, video-frames, viewfinder) | `Image` met `Handle<Image>` / `<img>`/`<video>`; decode gaat naar `platform::workers`, wat nu niet overal zo is |
| `compact()` 25× | één `Breakpoint`-signaal |
| `motion.rs` op `ctx.animate_*` + stille-frame-overslag | `motion` schrijft `Node`-waarden via signalen; geen repaint-boekhouding; "sluiten is altijd een knip" verdwijnt omdat *gaan* net zo'n overgang is als *komen* |
| `every_root_ui_panel_is_in_the_chain`, `EguiMut`-kliek (68 systemen serieel) | geen gedeelde root-`Ui`; panelen zijn bomen, systemen schrijven signalen en parallelliseren weer |
| MCP `ui_tree`/`press` via AccessKit uit egui | de boom *is* de component-boom: stabiele ids, rects uit `ComputedNode`/`getBoundingClientRect`, dezelfde `press` |
| `web_theme.rs` genereert kleuren; `controls.rs` tekent maten na | alles gegenereerd uit één tabel; de `controls.rs`-tabel verdwijnt |
| `source_rules` (knoppen/velden/checkboxes zijn de theme's) | overbodig: er is geen ander pad dan de componenten |

Wat het ontwerp *niet* oplost: de 9 pagina-stapels (fluid-ui oorzaak 6) worden
pas één `Page`-component als de panelen overgaan; de tekstkwaliteit op de
telefoon hangt aan Bevy 0.21 (§7); en de 34 headless-kliktests die nu een
`egui::Context` draaien worden tests op de component-boom.

## 6. Afgewezen, en waarom

- **Dioxus + Blitz via bedina.** Het dichtst bij "één code, echte CSS", en de
  enige Bevy-koppeling die de makers zelf onderhouden. Maar: Blitz is bèta met
  18–24 ms frames op scrollen, Stylo brengt MPL en ~1,5 miljoen regels
  afhankelijkheid in elke clientbuild, de koppeling leeft alleen op Dioxus
  0.8-*alpha*, Blitz' CSS-status pagina en README spreken elkaar tegen over
  custom properties, en de wasm-wereld zou wéér op een canvas tekenen. Een
  weddenschap op 2027, niet een vervanging nu. Wat we overnemen: de
  template-met-gaten als latere macro-uitbreiding.
- **Leptos forken (tachys met cfg-swap) + eigen Bevy-renderer.** De swap is
  mechanisch bewezen (Blitz-spike 2026-10-27), maar nooit naar een target dat
  zelf lay-out doet; `Renderer: Send` botst met `!Send` DOM- en
  `World`-handles onder atomics; één onderhouder; en we zouden alsnog
  lay-out, stijl en invoer voor Bevy zelf schrijven — het grote deel.
- **Masonry in Bevy + `xilem_web`.** De host-vorm van `masonry_core` is goed,
  maar: geen release sinds 2025-10-29, eigen lay-out die in januari 2026
  herschreven werd, wasm-build kapot, kleine widgetset, en het web-deel is
  een tweede vocabulaire — dus tóch twee keer schrijven.
- **iced / GPUI / Slint.** iced: geen toegankelijkheid, geen Android, geen
  DOM. GPUI: bezit het venster, geen mobiel, web zonder tekst. Slint:
  canvas op het web, licentie met badge-plicht.
- **egui houden en doorgaan met schermrondes** (het besluit van 2026-11-22).
  Goedkoop en de rondes blijven zinvol tot de panelen overgaan, maar het
  beantwoordt de vraag niet: twee implementaties blijven twee.
- **egui forken.** Zou per-widget stijl kunnen geven, maar niets voor het web.
  (De oude reden "een fork koopt alleen hinting" is achterhaald: egui 0.34
  heeft hinting sinds 2026-03.)
- **Bun als runtime in de client voor TS-componenten.** Geen Android, geen
  wasm.

## 7. Risico's en wat eerst gemeten wordt

1. **Bevy 0.20 tekent tekst zonder sub-pixelpositionering** (verloren bij de
   Parley-migratie, PR #26010 herstelt het op `main` 2026-11-17) en legt de
   hele UI-boom per frame uit (incrementeel in 0.21, PR #25653). Op een
   450-ppi telefoon weegt het eerste weinig, op een laptop wel. **Fase 0
   fotografeert** een `bevy_ui`-paneel naast egui 0.36 op 412×900 / 1,5× en
   op 1600×900 / 1×, op mac-2 (een CPU-node zegt niets over tekst). Als het
   zichtbaar slechter is, start de Bevy-backend op Bevy 0.21.
2. **Bouwkosten.** `bevy_ui` + `bevy_text` + `bevy_ui_render` + `bevy_picking`
   zijn eenmalige afhankelijkheden; de terugkerende kosten zijn de
   `Query<&Node…>`-systemen die wij schrijven (`client-build-cost.md`:
   monomorfisatie). Fase 0 meet `cargo build --timings` met de features aan.
3. **Sleep-scrollen, traagheid, tik-of-sleep** zijn in `bevy_ui` niet
   ingebouwd; egui's `predicted_dt`-fling was net gerepareerd. De backend
   schrijft het zelf, één keer, in `Scroll`.
4. **Web-a11y vanuit het frame** is er nu ook niet (AccessKit heeft geen
   web-adapter); de DOM-backend geeft het gratis.
5. **Eigen reactieve kern en macro** zijn werk dat anderen al deden. Ze zijn
   klein (de fork's signalen zijn 529 regels TS; een `view!` op rstml is
   honderden regels), maar het is ónze code om te onderhouden. Het
   alternatief (`reactive_graph`) staat klaar.
6. **Twee UI-lagen tegelijk** tijdens de migratie: wie krijgt de aanraking?
   Regel: de nieuwe laag eerst, egui daarna, de wereld als laatste;
   `owns_click` leest beide. Dev-panelen gaan als laatste over.
7. **De TS-kant.** midas' hub blijft Mithril en deelt alleen CSS; dat is
   geen verlies tegenover nu. De CMS-SSR gaat van Bun naar Rust-string pas
   als de `cms`-componenten over zijn; tot dan blijft `pages-ssr.js`.
8. **Omvang.** 78.012 regels UI in de client zijn niet in weken over. Het
   plan landt per paneel; de stille-frame-sloot, de panel-chain en egui's
   brug verdwijnen pas aan het eind.

## 8. Fasering — elke stap landt alleen en laat iets zien

0. **Meten** (een sessie): `bevy_ui`-tekst naast egui op de telefoon- en
   desktopmaat (mac-2), `--timings` met de UI-features aan, en een
   `bevy_ui`-proef met 2.000 knopen op de telefoon-tier voor de lay-outkost.
   Uitkomst: Bevy 0.20 nu, of de Bevy-backend wacht op 0.21.
1. **Kern + macro + DOM-backend + tokens** (eigen repository, zoals de
   mithril-fork; crate-naam open). Bewijs: één bestaande atlas-webpagina
   (`/tokens` of `/register`, klein, formulier-zwaar) in Rust/wasm, die in
   headless Chrome **pixelgelijk** is aan de TS-versie met de bestaande
   `components.css`, plus dezelfde HTML als string (SSR). De
   `web_theme.rs`-generatie verhuist naar de bibliotheek.
2. **Componentstijlen in Rust → `components.css` gegenereerd**, component
   voor component, met de fork's sandbox als vergelijkingsbeeld. Daarna is
   de TS-fork alleen nog renderer; zijn CSS komt uit Rust.
3. **Bevy-backend** op het gekozen Bevy: primitieven → `Node`, picking,
   `TextInput`, `Layer`, `Scroll` met traagheid. Eerste scherm: **Settings**
   (formulieren, geen painter), naast egui, gefotografeerd met
   `docs/phone-ui-shots.toml` tegen de web-pagina ernaast.
4. **Het wereldframe op de DOM-backend**: zoeker, lagen, Places, kaartjes,
   pins. Hier wordt de vraag letterlijk waar: wasm-wereld + web-componenten,
   één stijl met de site.
5. **Paneel voor paneel** in de native app, zwaarste eerst (`chats.rs` 5.318,
   `settings.rs`, `data_layers.rs`, `camera_screen.rs`), `Canvas` voor de
   painter-schermen (kompas, OSD, cosmos, gezichten), de dev-panelen als
   laatste; dan de egui-brug, de panel-chain-test en `source_rules` eruit.
6. **Optioneel**: Pinnacle-backend; `cms`-SSR naar Rust; midas' hub over.

Fase 0 en 1 kosten enkele sessies en veranderen niets aan wat live staat.
Fase 3–5 is het programma van maanden; elke landing is een scherm dat er in
app én web hetzelfde uitziet.

## Bronnen

De data en URL's staan per kandidaat in `landscape-2026-11.md`; de tellingen
per bestand in `egui-inventory.md` en `web-inventory.md`. Alles gelezen op
2026-11-23 uit `main` (d0103e24), de fork op 5ef10eb3, midas en podium op hun
`main`.
