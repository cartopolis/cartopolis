---
topic: component-library
triggers: ["component library", "componentbibliotheek", "jsx", "rsx", "view!", "replace egui", "egui vervangen", "egui weg", "Dioxus", "Leptos", "Yew", "Xilem", "Masonry", "Vello", "bevy_ui", "bevy_feathers", "bevy_ui_widgets", "bevy_flair", "signals", "reactive", "design tokens", "@garage44/mithril", "mithril", "components.css", "web_theme.rs", "theme/controls.rs", "ui-vocabulary", "one design", "same style", "dezelfde stijl", "toolkit", "ui_probe", "CARTO_UI_PROBE", "ui-probe"]
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
- **Eerst meten, dan bouwen — gemeten (§9); fase 1 gebouwd (§8.1).** Twee feiten beslisten of
  `bevy_ui` nu al kan: Bevy 0.20 verloor sub-pixelpositionering van tekst bij
  de overgang naar Parley (hersteld op `main` 2026-11-17 voor 0.21) en legt de
  hele UI-boom elke frame opnieuw uit (incrementeel in 0.21). Fase 0
  fotografeerde `bevy_ui`-tekst naast egui 0.36 op 412×900 / 1,5× en 1600×900:
  op de telefoon vrijwel gelijk, op desktop 1× iets lichter; 6.000 knopen
  kosten ≤ 3 ms; de features kosten ~9 min CPU eenmalig per schone build.
  **De Bevy-backend kan op 0.20 beginnen.** Dan fase 1: kern + macro +
  DOM-backend, bewezen op één atlas-webpagina die pixelgelijk uit Rust komt.
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

0. **Meten** — gedaan 2026-11-23, §9: `bevy_ui`-tekst naast egui op de
   telefoon- en desktopmaat (mac-2), `--timings` met de UI-features aan, en
   2.000 rijen voor de lay-outkost. Uitkomst: Bevy 0.20 nu.
1. **Kern + macro + DOM-backend** — gebouwd 2026-11-23, **in
   `jeroen/mithril` zelf**: de eigenaar koos "vervang de huidige
   jeroen/mithril met ons nieuwe systeem", dus de Rust-editie staat in
   `crates/` naast de TypeScript in `src/`, tot die uitgefaseerd is.
   Signalen, de view-kern met `Renderer`-trait en string-renderer, `view!`
   op rstml, de DOM-backend, en 19 componenten als port. Het bewijs werd
   niet één atlas-pagina maar **de hele componentenset**:
   `tools/parity.ts` rendert elk specimen met de TS-serverrenderer en met
   de Rust-stringrenderer en vergelijkt de HTML (24 van 24 gelijk);
   `tools/parity-web.sh` bouwt dezelfde specimens als twee pagina's over
   één `components.css`, in headless Chromium op 800×2400 pixelgelijk op de
   animatiefase van twee spinners na (414 van 1,9 miljoen pixels). Nog niet:
   `validation`, `FieldTags`, `Table`, `Tabs`, `Collapse`, `Dialog`,
   `Toaster`, `ThemeSwitch`, `Shell`; de `web_theme.rs`-generatie verhuist
   in fase 2. Het ontwerp staat in de repository's `docs/notes/rust.md`.

   ![pariteit](fase1-pariteit.png)
2. **Componentstijlen in Rust → `components.css` gegenereerd** — gebouwd
   2026-11-23. `crates/style` houdt de tokens getypt (`Tokens`, elke kleur
   een licht/donker-paar) en de regels als `css!`-blokken, data die een
   native backend kan doorlopen; `gen-css` schrijft de vijf stylesheets van
   de TS-editie en de poort faalt op een verouderd bestand. De eerste
   generatie was na minificatie byte-gelijk aan de handgeschreven files.
   Daarna de rest van de bibliotheek: validatie (`mithril_validation`),
   FieldTags, Table, Tabs, Collapse, Dialog, Toaster, ThemeSwitch, Shell —
   elk component heeft nu zijn tweeling, 34 specimens gelijk in HTML en de
   twee pagina's **pixelgelijk** (0 van 3,36 miljoen pixels, animaties uit).
   Nog niet: cartopolis' eigen `Tokens` in plaats van `web_theme.rs` (komt
   met fase 4, als cartopolis de bibliotheek gaat gebruiken).

   ![pariteit fase 2](fase2-pariteit.png)
3. **Bevy-backend** op het gekozen Bevy — eerste stap (3a) gebouwd
   2026-11-23. De cascade zit in `crates/style` zelf (`Stylist`, Bevy-vrij):
   hij loopt dezelfde `css!`-regels door die `gen-css` schrijft, met
   selectors, `&`-nesting, custom properties, `calc`, `color-mix`, de
   coarse-pointer- en breedtecondities en een inline `style=` als laatste.
   De renderer, `crates/mithril_bevy` in cartopolis (verhuist naar
   `jeroen/mithril` als de vorm staat), legt de boom in `bevy_ui`-entiteiten
   via een op-wachtrij: een signaal schrijft, het effect zet ops in de rij,
   `drive_ui` past ze per frame toe en stijlt wat vuil is; picking en
   toetsenbord gaan als `Event` naar de handlers. Wat het stylesheet met
   pseudo-elementen tekent, tekent de renderer zelf (vinkje, radiopunt,
   schakelknop, sluitkruis, chevron, de draai van de spinner). Bewijs:
   dezelfde pagina (`CARTO_UI_PROBE=mithril`) in Bevy en in de browser —
   dezelfde HTML uit de string-renderer met dezelfde stylesheets — op
   telefoon 412×900 @1.5 en desktop 1600×900. Twee vallen van Bevy 0.20
   staan bij de code: `UiTransform` nooit van een node halen, `Children`
   nooit verwijderen-en-toevoegen; beide laten de hele boom 0×0.

   ![Bevy naast de browser](fase3-bevy.png)

   **3b — Settings als eerste scherm naast egui**, gebouwd 2026-11-24
   (`CARTO_UI_LIB=settings`, `systems::lib_settings`;
   `docs/notes/component-library-in-the-client.md`). De bibliotheek kreeg
   er de woordenschat van een instellingenpagina bij — `Section`, `List`,
   `ListItem` (icoon op een vierkantje, titel, regel eronder, woord of
   chevron rechts), in beide edities met één markup. De lijst volgt
   mockup D van `home-and-profile/v1`; de pagina General is overgezet met
   zijn voorkeuren twee kanten op gebonden (`AppPrefs` ↔ signalen, per
   frame een diff); de andere pagina's zeggen dat ze nog egui's zijn. egui
   houdt een lege paneelstrook vast, zodat dock, pins en kaartrect opzij
   gaan zoals bij elk paneel, en de bibliotheek tekent erin. Foto: mockup,
   Bevy en browser naast elkaar op de telefoon; de desktop eronder.

   ![Settings op de bibliotheek](fase3b-settings.png)

   Daarna (2026-11-24) de stukken die de overige pagina's nodig hebben, in
   de renderer: de slider (Bevy's headless `Slider` onder een getekende
   baan, vulling en knop), de select-popup (een backdrop en een menu die de
   renderer zelf als view monteert, met regels in `fields.rs`),
   placeholder-tekst (bevy_text tekent die nog niet) en scrollen met de
   vinger; en de pagina's **Graphics** en **World** overgezet, met hun
   `Option`-voorkeuren als vinkje-plus-"Automatic" en `DevPanelState` en
   het weer twee kanten op gebonden.

   ![Graphics en World op de bibliotheek](fase3b-pages.png)

   Ten slotte **Sound and voice, Account, Privacy and safety en Access
   tokens** (2026-11-24): daarmee staat elke Settings-pagina op de
   bibliotheek. Een handeling — de toon, de microfoon, verwijderen,
   deblokkeren, een token maken of intrekken, kopiëren — is een `Cell` die
   de view zet en het systeem opneemt, waar de resources en de verbinding
   zijn; wat getoond wordt (de meter, de tokenlijst, een fout) spiegelt het
   systeem elk frame in een signaal. Nog niet: de cameratest (een live
   beeld), op General de ontvanger, opslag en editor-toegang, en
   cartopolis' eigen `Tokens` (fase 4).

   ![De overige pagina's](fase3b-rest.png)
4. **Het wereldframe op de DOM-backend**: zoeker, lagen, Places, kaartjes,
   pins. Hier wordt de vraag letterlijk waar: wasm-wereld + web-componenten,
   één stijl met de site.

   **4a — één tokentabel**, gebouwd 2026-11-24. `theme::tokens()` is
   cartopolis' eigen `Tokens` (de bibliotheek's getypte struct): het donkere
   deel het palet van de client zoals het web het tekent, het lichte deel
   het web's eigen. `web_theme.rs` schrijft `theme.css` er nu uit (het
   bestand kwam byte-gelijk terug, dus de oude handmatige koppeling is
   exact overgenomen) en de Bevy-backend tekent ermee: Settings in de app
   draagt het palet van de site — dezelfde teal, dezelfde vlakken — in
   plaats van de oranje bibliotheekkleuren. Open: de app's eigen accent is
   sinds 2026-11-10 dieper teal met wit erop, het web hield de heldere
   teal met donkere inkt; welke de ene tabel krijgt is een keuze voor de
   eigenaar.

   ![Eén tokentabel](fase4-tokens.png)

   **4b — dezelfde schermen als DOM in de webbuild**, gebouwd 2026-11-24.
   `systems::lib_host` is de ene naad: een scherm roept `mount`/`clear`
   aan en de lane kiest — `ui-probe` tekent via `mithril_bevy`, `ui-dom`
   (wasm) via `mithril::dom` in een `<div>` over het canvas, met de
   stylesheets van de bibliotheek uit `theme::tokens()` in de pagina
   geschreven. Zo is Settings in het wereldframe op cartopolis.org echte
   DOM in de stijl van de site (`?ui=lib&panel=settings`). De schermen
   zelf — alle Settings-pagina's — staan er één keer; wat per backend
   verschilt (camera, fonts, schaal, wie de klik heeft) zit achter een
   `cfg`. Geen foto: de agent-container heeft geen GPU (ANGLE-Vulkan
   vindt geen adapter, SwiftShader verliest het device vóór het eerste
   frame, `web-device-loss.md`) en de Mac-nodes geen browser; de
   wasm-check van de poort compileert de lane. Te zien op
   `cartopolis.org/viewer/?ui=lib&panel=settings` zodra de webclient
   opnieuw is gepubliceerd.
5. **Paneel voor paneel** in de native app, zwaarste eerst (`chats.rs` 5.318,
   `settings.rs`, `data_layers.rs`, `camera_screen.rs`), `Canvas` voor de
   painter-schermen (kompas, OSD, cosmos, gezichten), de dev-panelen als
   laatste; dan de egui-brug, de panel-chain-test en `source_rules` eruit.
6. **Optioneel**: Pinnacle-backend; `cms`-SSR naar Rust; midas' hub over.

Fase 0 en 1 kosten enkele sessies en veranderen niets aan wat live staat.
Fase 3–5 is het programma van maanden; elke landing is een scherm dat er in
app én web hetzelfde uitziet.

## 9. Fase 0 — gemeten op 2026-11-23

Besloten dezelfde middag: richting §4 (eigen bibliotheek over `bevy_ui` en de
DOM), en fase 0 meteen. Gemeten op **mac-2** (Apple M5 Pro, een VM met echte
GPU) met `systems::dev::ui_probe`: een dev-only sonde achter de cargo-feature
`ui-probe` (die `bevy_ui`, `bevy_ui_render`, `bevy_ui_widgets`, `ui_picking`
en `bevy_input_focus` aanzet), die op `CARTO_UI_PROBE=1` de eerste
Settings-controls als `Node`-boom tekent — titel, sectiekop, gelabeld
tekstveld met `TextInput`, alinea, primaire en ghost-knop, schakelaar, drie
lijstrijen — met de maten en kleuren van `theme::controls`, en op
`CARTO_UI_PROBE=2000` zoveel rijen in één scrollende kolom.

```bash
# dezelfde foto's: egui's Settings, dan de sonde met egui verborgen
CARTO_UI_THEME=dark CARTO_UI_SCALE=1.5 cargo run -p cartopolis --features ui-probe -- \
    --shots docs/phone-ui-shots.toml --only settings
CARTO_UI_PROBE=1 CARTO_UI_THEME=dark CARTO_UI_SCALE=1.5 cargo run -p cartopolis --features ui-probe -- \
    --shots <manifest met hide_ui = true> --only none
```

**Tekst.** Op de telefoonmaat (412×900 @1,5) is het verschil op pixelniveau
klein: egui 0.36 (hinting via vello_cpu) zet iets scherper en zwaarder,
Parley in Bevy 0.20 iets zachter — zonder sub-pixelpositionering — maar
even leesbaar. Op desktop 1× bij 12,5–14 pt is Bevy's tekst lichter van
gewicht dan egui's; niet onleesbaar, wel zichtbaar naast elkaar. **Besluit:
de Bevy-backend kan op 0.20 beginnen**; de overgang naar 0.21 (sub-pixel
terug, incrementele lay-out) is een upgrade onderweg, geen voorwaarde.

![telefoon](fase0-telefoon.png)
![desktop](fase0-desktop.png)
![tekst op 300 %](fase0-tekst-300pct.png)

**Bouwkosten.** `cargo build -p cartopolis --timings`, dev-profiel, mac-2:

| | wandklok | eenheden |
|---|---|---|
| zonder feature, warm | 70,8 s | 4 |
| met `ui-probe`, na de wissel | 556,1 s | 115 (2.681 s CPU) |
| waarvan de **nieuwe** crates (23) | — | **518 s CPU**: `bevy_ui` 113, `bevy_sprite_render` 111, `bevy_ui_widgets` 61, `bevy_picking` 59, `bevy_text` 57, `bevy_ui_render` 55, `bevy_sprite` 32, `swash` 16, parley+fontique+taffy+icu samen < 15 |

De rest van de 556 s is de feature-*wissel*: een feature op de `bevy`-paraplu
hercompileert `bevy_render`, `bevy_pbr` en alles erboven, cartopolis zelf
incluis (142,7 s). Dat is eenmalig; met de features vast aan kost een schone
build ~9 min CPU extra over alle kernen (~1–2 min wandklok), en een edit in
de client niets extra's tot er `Query<&Node…>`-systemen bij komen
(`client-build-cost.md`).

**Lay-outkost.** 2.000 rijen (6.001 knopen: rij, stip, tekst) in één
scrollende kolom, telefoonmaat, `CARTO_FPS_CAP=0` zodat de stille-frame-rem
niet meeweegt, veertig seconden per run, twee runs elk: de beste seconden
liggen voor de basis op 27–30 ms per frame en met 2.000 rijen op 30 ms;
500 rijen 31 ms. Het verschil valt binnen de ruis van een frame dat de
wereld zelf al 30 ms kost op deze VM, dus **≤ 3 ms voor 6.000 knopen die
elke frame opnieuw gelegd worden**; de telefoon is niet gemeten (geen APK met
de feature). Fijner meten kan met de `perf`-feature per systeem
(`ui_layout_system`), als het ooit het verschil maakt.

Wat de sonde níet is: een component. Hij schrijft de controls nog eens als
`Node`s; de bibliotheek van §4 is wat ze één keer definieert.

## Bronnen

De data en URL's staan per kandidaat in `landscape-2026-11.md`; de tellingen
per bestand in `egui-inventory.md` en `web-inventory.md`. Alles gelezen op
2026-11-23 uit `main` (d0103e24), de fork op 5ef10eb3, midas en podium op hun
`main`.
