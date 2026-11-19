use super::*;

fn spec(lat: f64, lng: f64, alt: f64) -> ShotSpec {
    ShotSpec {
        lat,
        lng,
        alt,
        pitch_deg: -90.0,
        yaw_deg: 0.0,
        ..default()
    }
}

/// A one-pixel line is the aliasing crawl a 5 cm nudge legitimately causes
/// on every silhouette; a patch is a surface repainted in another's colour.
/// The erosion is the whole difference between an instrument and a noise
/// meter, so it gets its own pin.
#[test]
fn solid_flips_counts_patches_not_edge_crawl() {
    let (w, h) = (16usize, 16usize);
    let base = vec![0u8; w * h * 3];
    // A 5×5 patch of a different colour: 3×3-solid interior = 3×3 = 9.
    let mut patch = base.clone();
    for y in 4..9 {
        for x in 4..9 {
            patch[(y * w + x) * 3] = 255;
        }
    }
    assert_eq!(solid_flips(w, h, &base, &patch), 9);
    // A one-pixel-wide line of any length: no 3×3-solid pixel at all.
    let mut line = base.clone();
    for y in 0..h {
        line[(y * w + 8) * 3] = 255;
    }
    assert_eq!(solid_flips(w, h, &base, &line), 0);
    // Sub-threshold change is not a flip.
    let dim = vec![FLIP_THRESHOLD; w * h * 3];
    assert_eq!(solid_flips(w, h, &base, &dim), 0);
    // Degenerate sizes and mismatched buffers answer 0, never panic.
    assert_eq!(solid_flips(2, 2, &[0; 12], &[255; 12]), 0);
    assert_eq!(solid_flips(w, h, &base, &base[..3]), 0);
}

#[test]
fn flicker_doubles_the_shots_into_nudged_pairs() {
    let h = ShotHarness::from_cli(ShotArgs {
        path: PathBuf::from("/tmp/f.png"),
        at: &["53.0,6.0,100".into(), "53.1,6.1,200".into()],
        flicker: true,
        ..default()
    })
    .expect("valid");
    assert_eq!(h.shots.len(), 4);
    assert_eq!(h.shots[0].lat, 53.0);
    assert!((h.shots[1].lat - (53.0 + FLICKER_NUDGE_DEG)).abs() < 1e-12);
    assert_eq!(h.shots[1].lng, h.shots[0].lng);
    assert_eq!(h.shots[2].lat, 53.1);
}

/// Without `--flicker` the metric is a constant 0 and any `flicker_px`
/// expectation holds vacuously — a green gate that measured nothing. That
/// has to be a startup error, same as an unknown key.
#[test]
fn a_flicker_expectation_without_flicker_is_refused() {
    let err = ShotHarness::from_cli(ShotArgs {
        path: PathBuf::from("/tmp/f.png"),
        at: &["53.0,6.0,100".into()],
        expect: &["flicker_px<=200".into()],
        ..default()
    })
    .err()
    .expect("must refuse");
    assert!(err.contains("--flicker"), "{err}");
}

#[test]
fn densify_passthrough_when_off_or_single() {
    let wp = vec![spec(0.0, 0.0, 2000.0), spec(1.0, 1.0, 4000.0)];
    assert_eq!(
        densify_path(&wp, 0).len(),
        2,
        "frames=0 leaves waypoints as-is"
    );
    let one = vec![spec(0.0, 0.0, 2000.0)];
    assert_eq!(
        densify_path(&one, 8).len(),
        1,
        "a single waypoint can't be swept"
    );
}

#[test]
fn densify_hits_both_endpoints() {
    let wp = vec![spec(10.0, 20.0, 2000.0), spec(11.0, 22.0, 4_000_000.0)];
    let out = densify_path(&wp, 16);
    assert_eq!(out.len(), 16);
    // First and last samples are the waypoints themselves.
    assert!((out[0].lat - 10.0).abs() < 1e-9 && (out[0].alt - 2000.0).abs() < 1e-6);
    assert!((out[15].lat - 11.0).abs() < 1e-9 && (out[15].alt - 4_000_000.0).abs() < 1.0);
}

#[test]
fn altitude_is_geometric_not_linear() {
    // Midpoint of a 2 km → 200 km sweep: geometric mean (20 km), not the
    // linear midpoint (101 km). This is what keeps samples off the ceiling.
    let wp = vec![spec(0.0, 0.0, 2_000.0), spec(0.0, 0.0, 200_000.0)];
    let out = densify_path(&wp, 3);
    assert!((out[1].alt - 20_000.0).abs() < 1.0, "got {}", out[1].alt);
}

#[test]
fn densify_spans_multiple_segments() {
    // Three waypoints, even frame count: the middle waypoint is passed through
    // exactly at the sweep's centre.
    let wp = vec![
        spec(0.0, 0.0, 1000.0),
        spec(5.0, 0.0, 1000.0),
        spec(10.0, 0.0, 1000.0),
    ];
    let out = densify_path(&wp, 5);
    assert_eq!(out.len(), 5);
    assert!(
        (out[2].lat - 5.0).abs() < 1e-9,
        "centre sample is the middle waypoint"
    );
}

#[test]
fn frames_requires_two_waypoints() {
    let result = ShotHarness::from_cli(ShotArgs {
        path: "/tmp/s.png".into(),
        at: &["0,0,1000".into()],
        frames: 8,
        ..default()
    });
    match result {
        Err(e) => assert!(e.contains("--frames"), "got {e:?}"),
        Ok(_) => panic!("expected --frames with one waypoint to error"),
    }
}

/// A clip run is one cycle per viewpoint, flown to once: only the first
/// frame of each moves the camera, and the phases never photograph the
/// loop's end, which is its start.
#[test]
fn a_clip_is_one_cycle_per_viewpoint() {
    let h = ShotHarness::from_cli(ShotArgs {
        path: "/tmp/walk.png".into(),
        at: &["0,0,4".into(), "0,0,6".into()],
        clip: Some("walk"),
        clip_frames: 4,
        ..default()
    })
    .expect("a clip run");
    assert_eq!(h.mode, "avatar", "--clip implies the avatar camera");
    assert_eq!(h.shots.len(), 8);
    let phases: Vec<f32> = h.shots.iter().map(|s| s.clip_phase.unwrap()).collect();
    assert_eq!(phases, [0.0, 0.25, 0.5, 0.75, 0.0, 0.25, 0.5, 0.75]);
    let flown: Vec<bool> = h.shots.iter().map(|s| !s.in_place).collect();
    assert_eq!(
        flown,
        [true, false, false, false, true, false, false, false]
    );
    assert_eq!(h.shots[4].alt, 6.0, "the second viewpoint is its own");
    assert_eq!(h.clip_sheet_path(), PathBuf::from("/tmp/walk_sheet.jpg"));
}

#[test]
fn a_clip_refuses_what_it_cannot_photograph() {
    let with = |clip, mode: &str, flicker| {
        ShotHarness::from_cli(ShotArgs {
            path: "/tmp/c.png".into(),
            at: &["0,0,4".into()],
            clip: Some(clip),
            mode: mode.into(),
            flicker,
            ..default()
        })
    };
    assert!(with("moonwalk", "free", false).is_err_and(|e| e.contains("walk, run")));
    assert!(with("walk", "earth", false).is_err_and(|e| e.contains("--shot-mode")));
    assert!(with("walk", "avatar", true).is_err_and(|e| e.contains("--flicker")));
    assert!(
        with("Wave", "avatar", false).is_ok(),
        "names are case-blind"
    );
}

/// The sheet wraps a cycle at six columns, stacks viewpoints, and keeps a
/// missing frame's place.
#[test]
fn the_clip_sheet_keeps_every_frame_in_its_place() {
    let frame = |v| Some(image::RgbImage::from_pixel(480, 640, image::Rgb([v, v, v])));
    let mut frames: Vec<_> = (0..16).map(|i| frame(i as u8 * 10)).collect();
    frames[3] = None;
    let sheet = clip_sheet(&frames, 8).expect("frames landed");
    let (tw, th) = (SHEET_TILE_W, 320);
    assert_eq!(sheet.width(), 6 * (tw + SHEET_GAP) + SHEET_GAP);
    let block_h = 2 * (th + SHEET_BAR_H + SHEET_GAP);
    assert_eq!(sheet.height(), SHEET_GAP + 2 * block_h + SHEET_BLOCK_GAP);
    // Frame 3 is grey, frame 7 (second row of the first block) is its own.
    let centre = |col: u32, y0: u32| {
        *sheet.get_pixel(SHEET_GAP + col * (tw + SHEET_GAP) + tw / 2, y0 + th / 2)
    };
    assert_eq!(centre(3, SHEET_GAP), image::Rgb([96, 96, 96]));
    assert_eq!(
        centre(1, SHEET_GAP + th + SHEET_BAR_H + SHEET_GAP),
        image::Rgb([70, 70, 70])
    );
    // Frame 8 opens the second block.
    assert_eq!(
        centre(0, SHEET_GAP + block_h + SHEET_BLOCK_GAP),
        image::Rgb([80, 80, 80])
    );
    assert!(clip_sheet(&[None, None], 2).is_none());
}

/// A live harness starts with nothing to do, and each viewpoint it is handed
/// brings its own patience — which is the difference between "re-photograph
/// what you are already looking at" and "fly to a city you have never been
/// to".
#[test]
fn a_live_harness_starts_empty_and_takes_patience_per_shot() {
    let mut h = ShotHarness::live_harness(UVec2::new(1280, 720), true);
    assert!(h.live);
    assert!(h.windowed);
    assert!(
        h.shots.is_empty(),
        "nothing to photograph until it is asked"
    );
    // Falls back to the run-wide values while nothing says otherwise.
    assert_eq!(h.settle_for(0), DEFAULT_SETTLE);
    assert_eq!(h.wait_for(0), DEFAULT_WAIT);

    h.shots.push(ShotSpec {
        settle: Some(0.0),
        in_place: true,
        out: Some(PathBuf::from("/tmp/here.png")),
        ..default()
    });
    assert_eq!(h.settle_for(0), 0.0, "the shot's own settle wins");
    assert_eq!(h.wait_for(0), DEFAULT_WAIT, "and the rest still falls back");
    assert_eq!(
        h.path_for(0),
        PathBuf::from("/tmp/here.png"),
        "a live shot names its own file, as a preset does"
    );
}

/// `harness_active` means "a script owns this run", and a windowed live
/// harness does not: the user store, the frame limiter and the load
/// controller all belong to the person whose session it is.
#[test]
fn a_windowed_live_harness_does_not_own_the_run() {
    let size = UVec2::new(640, 360);
    assert!(
        !ShotHarness::live_harness(size, true).owns_run(),
        "a client somebody is watching keeps its user store, its frame limiter \
             and its load controller"
    );
    assert!(
        ShotHarness::live_harness(size, false).owns_run(),
        "a windowless one is nobody's session"
    );
    assert!(
        ShotHarness::from_cli(ShotArgs {
            path: "/tmp/s.png".into(),
            at: &["0,0,1000".into()],
            ..default()
        })
        .unwrap()
        .owns_run(),
        "and a scripted run always has"
    );
}

#[test]
fn path_for_indexes_and_pads_a_sweep() {
    let h = ShotHarness::from_cli(ShotArgs {
        path: "/tmp/sweep.png".into(),
        at: &["0,0,2000".into(), "0,0,4000000".into()],
        frames: 12,
        ..default()
    })
    .unwrap();
    // 12 frames → indices 00..11, zero-padded to width 2, altitude-tagged.
    assert_eq!(h.path_for(0), PathBuf::from("/tmp/sweep_00_2000m.png"));
    assert!(h
        .path_for(11)
        .to_string_lossy()
        .starts_with("/tmp/sweep_11_"));
}

#[test]
fn manifest_run_names_its_own_files_and_carries_per_shot_state() {
    // The checked-in manifest is the docs' source of truth — if it stops
    // loading, `cargo shots` silently stops regenerating the README images.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/shots.toml");
    let h = ShotHarness::from_manifest(&path, &[], None, Some(0.1), Some(1.0), None, None, &[])
        .expect("checked-in docs/shots.toml must load");
    assert!(
        h.shots.len() >= 2,
        "manifest should describe a set of views"
    );
    for (i, s) in h.shots.iter().enumerate() {
        let name = s.name.clone().expect("preset shots are named");
        let out = h.path_for(i);
        assert!(
            out.to_string_lossy().contains(&name),
            "{name} should own its filename, got {}",
            out.display()
        );
        // The docs images live beside the manifest, not in the CWD.
        assert!(out.starts_with(path.parent().unwrap()), "{}", out.display());
    }
    // A bare run is the published set, so nothing it writes may sit in a
    // group's directory. The subject shots are reached by `--only <group>`.
    let m = crate::systems::shot_presets::Manifest::load(&path).unwrap();
    for g in m.groups() {
        for i in 0..h.shots.len() {
            let out = h.path_for(i);
            assert!(
                !out.to_string_lossy().contains(&format!("/{g}/")),
                "a bare `cargo shots` wrote into the {g} group: {}",
                out.display()
            );
        }
    }
    assert!(
        !m.groups().is_empty(),
        "the manifest should carry at least one subject group, or the \
             grouping machinery has no content and nothing exercises it"
    );
    // `--only` narrows the same set without changing anything else.
    let first = h.shots[0].name.clone().unwrap();
    let one =
        ShotHarness::from_manifest(&path, &[first.clone()], None, None, None, None, None, &[])
            .unwrap();
    assert_eq!(one.shots.len(), 1);
    assert_eq!(one.shots[0].name.as_deref(), Some(first.as_str()));
}

#[test]
fn manifest_size_can_be_overridden_from_the_cli() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/shots.toml");
    let h = ShotHarness::from_manifest(&path, &[], Some("640x360"), None, None, None, None, &[])
        .unwrap();
    assert_eq!(h.size, UVec2::new(640, 360));
}

/// A capture that measured nothing: every count zero, nothing settled, and
/// `ground_m` the `NaN` a run without the height layer really produces.
fn sample_metrics() -> ShotMetrics {
    ShotMetrics {
        shot: 1,
        name: Some("city".into()),
        lat: 0.0,
        lng: 0.0,
        camera_lat: 0.0,
        camera_lng: 0.0,
        aim_lat: 0.0,
        aim_lng: 0.0,
        flicker_px: 0,
        settled: false,
        // Two entries on purpose: `busy` is the one list-valued column, and
        // joining it with a comma would add columns to this row and silently
        // shift every header after it.
        busy: vec!["map_tiles".into(), "lod22".into()],
        settled_s: 1.0,
        frame_ms: 16.6,
        frame_ms_max: 42.0,
        draws: 0,
        tris_total: 0,
        tris_shadow: 0,
        shadow_casters: 0,
        tris_buildings: 0,
        tris_lod22: 0,
        tris_tiles: 0,
        tris_globe: 0,
        worker_outstanding: 0,
        fetch_outstanding: 0,
        radius_bias: 0,
        tiles_occluded: 0,
        aerial_tiles: 0,
        tiles_compressed: 0,
        tile_px_uploaded: 0,
        alt_m: 1000,
        space_factor: 0.0,
        in_space: false,
        orbit_space: false,
        near_m: 1,
        far_m: 1000,
        exposure_ev100: 14.0,
        bloom: 0.15,
        globe_albedo: 1.0,
        veg_factor: 0.0,
        grass_patches: 0,
        bag_buildings: 0,
        bag_addresses: 0,
        bag_berths: 0,
        surveyed_trees: 0,
        municipal_trees: 0,
        municipal_props: 0,
        surveyed_props: 0,
        surveyed_lines: 0,
        surveyed_paving: 0,
        crop_parcels: 0,
        nav_marks: 0,
        level_crossings: 0,
        prop_colliders: 0,
        flood_defences: 0,
        flood_defences_measured: 0,
        sewer_pipes: 0,
        sewer_meshes: 0,
        underground_shapes: 0,
        underground_meshes: 0,
        ground_unopened: 0,
        backdrop_present: false,
        backdrop_visible: false,
        backdrop_depth_m: 0.0,
        backdrop_half_m: 0.0,
        tunnel_count: 0,
        tunnel_road_m: 0.0,
        tunnel_meshes: 0,
        tunnel_rendered: 0,
        tunnel_entrance_lat: 0.0,
        tunnel_entrance_lng: 0.0,
        tunnel_entrance_heading: -1.0,
        tunnel_ahead_m: -1.0,
        tunnel_ahead_road_m: 0.0,
        basemap_open_tiles: 0,
        interior_rooms: 0,
        interior_storeys: 0,
        interior_designed: 0,
        cutaway_storey: -1,
        building_selected: false,
        building_indoors: false,
        building_shell_meshes: 0,
        building_shell_rendered: 0,
        building_doors: 0,
        building_scaffolds: 0,
        building_fences: 0,
        lod22_dated: 0,
        lod22_elements: 0,
        grass_slots: 0,
        grass_rendered: 0,
        prop_meshes: 0,
        traffic_cars: 0,
        traffic_walkers: 0,
        traffic_cyclists: 0,
        crows_air: 0,
        crows_ground: 0,
        furniture_factor: 0.0,
        globe_entities: 0,
        globe_visible: 0,
        globe_rendered: 0,
        globe_tiles_loaded: 0,
        globe_tiles_desired: 0,
        globe_texture_mb: 0.0,
        globe_tile_px: crate::systems::globe::GLOBE_TILE_PX,
        route_meshes: 0,
        route_rendered: 0,
        shaft_fog: false,
        shaft_volumes: 0,
        // A real number, not the `NaN` the no-volume case reports: this fixture
        // exists to prove the column is plumbed, and `ground_m` is already the
        // one field carrying the unmeasured-reads-as-null case.
        shaft_volume_m: 0.0,
        shaft_pref: false,
        shadows_pref: false,
        particle_emitters: 0,
        particle_quads: 0,
        satellites_drawn: 0,
        satellites_catalogued: 0,
        gizmo_render: false,
        gizmo_selected: 0,
        gizmo_axes_hit: 0,
        avatar_meshes: 0,
        avatar_floor_m: 0.0,
        eye_ground_m: 0.0,
        avatar_rendered: 0,
        ground_m: f32::NAN,
        height_patches: 0,
        fine_terrain_ready: false,
        fine_terrain_sample_m: 0.0,
        ground_detail_ready: false,
        ground_detail_sample_m: 0.0,
        water_rings: 0,
        water_carved_coarse: 0,
        water_carved_fine: 0,
        water_drop_m: 0.0,
        terrain_ready: false,
        terrain_relief_m: 0.0,
        terrain_sample_m: 0.0,
        terrain_graded_samples: 0,
        approach_abutments: 0,
        approach_raised: 0,
        deck_count: 0,
        decks_unapproached: 0,
        deck_step_max_m: 0.0,
        deck_step_mean_m: 0.0,
        deck_profiled: 0,
        deck_piers: 0,
        upload_peak_kb: 0,
        upload_peak_asset_kb: 0,
        gpu_resident_kb: 0,
        gpu_resident_peak_kb: 0,
        warm_tile_hits: 0,
        warm_tile_mb: 0.0,
        warm_cell_restores: 0,
        warm_cell_mb: 0.0,
        software_renderer: true,
        vram_allocated_kb: 0,
        vram_reserved_kb: 0,
        gpu_buffers_kb: 0,
        gpu_textures_kb: 0,
        fetch_count: 0,
        fetch_errors: 0,
        fetch_mb: 0.0,
        fetch_ttfb_ms: 0,
        fetch_body_ms: 0,
        worker_jobs: 0,
        worker_busy_ms: 0,
        worker_wait_ms: 0,
        // Two entries for the same reason as `busy` above: this is the other
        // list-valued column, and the `|` join is what keeps it one cell.
        worker_split: vec!["map_tiles:2:10:5".into(), "lod22:1:8:3".into()],
        worker_threads: 0,
        worker_utilisation: 0.0,
    }
}

/// [`UI_ELEMENTS`] and `open_scripted_ui`'s handlers are two lists that have
/// to agree, and nothing in the type system relates them — a handler arm is a
/// string comparison against a literal. They drifted for real: `settings` and
/// `graphics` were handled for months while the vocabulary called them
/// unknown, so `--ui graphics` did its job and logged that it hadn't.
///
/// So the test reads this file back and checks the one direction that matters.
/// Unusual, and cheaper than the alternative: the only other way to relate
/// them is to run the system, which needs a built `App` with two dozen
/// resources in it.
#[test]
fn ui_vocabulary_covers_every_handler() {
    // Assembled rather than written out, so this needle does not match the
    // line that defines it — the scan reads the whole file, this one included.
    let call = format!("want({}", '"');
    let source = include_str!("../shot_harness.rs");
    let mut missing: Vec<&str> = source
        .match_indices(&call)
        .map(|(i, _)| {
            let rest = &source[i + call.len()..];
            &rest[..rest.find('"').expect("unterminated --ui handler name")]
        })
        .filter(|name| !UI_ELEMENTS.contains(name))
        .collect();
    missing.sort_unstable();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "open_scripted_ui handles {missing:?}, which UI_ELEMENTS does not list \
             — `--ui` would warn that they were ignored while acting on them"
    );
    // And the scan found the handlers at all, so a rename of `want` can't turn
    // this into a test that passes by looking at nothing.
    assert!(
        source.matches(&call).count() > 10,
        "found no --ui handlers to check"
    );
}

#[test]
fn csv_header_and_row_have_matching_arity() {
    assert_eq!(
        CSV_HEADER.split(',').count(),
        sample_metrics().csv_row().split(',').count(),
        "CSV header and row column counts drifted"
    );
}

/// The arity test above is necessary and was never sufficient: it passed for a
/// header that named a field the struct doesn't have, because the missing cell
/// is still a cell. Rows are built by name now, so *that* is the failure left
/// to guard — a renamed or removed field turns its column into blanks for
/// every row of every run, and nothing else says so.
#[test]
fn csv_columns_all_resolve() {
    let value = serde_json::to_value(sample_metrics()).unwrap();
    for key in CSV_HEADER.split(',') {
        assert!(
            value.get(key).is_some(),
            "CSV column {key:?} names no ShotMetrics field"
        );
    }
}

/// Columns land under their own headers, and the cells that aren't plain
/// numbers keep their shape: `busy` stays one `|`-joined cell rather than
/// spilling into the columns after it, and a `NaN` metric is blank — the same
/// "not measured" `measure()` reports for it.
#[test]
fn csv_cells_line_up_with_their_headers() {
    let m = sample_metrics();
    let header: Vec<&str> = CSV_HEADER.split(',').collect();
    let row = m.csv_row();
    let cells: Vec<&str> = row.split(',').collect();
    let cell = |key: &str| {
        cells[header
            .iter()
            .position(|h| *h == key)
            .unwrap_or_else(|| panic!("{key} is not a column"))]
    };
    assert_eq!(cell("shot"), "1");
    assert_eq!(cell("name"), "city");
    assert_eq!(cell("busy"), "map_tiles|lod22");
    assert_eq!(cell("settled"), "false");
    assert_eq!(cell("alt_m"), "1000");
    assert_eq!(
        cell("space_factor"),
        "0.0000",
        "float precision is per column"
    );
    assert_eq!(cell("frame_ms"), "16.60");
    assert_eq!(cell("ground_m"), "", "a NaN metric reads as unmeasured");
    assert_eq!(cell("software_renderer"), "true");
    // The last column is populated, i.e. nothing shifted off the end.
    assert_eq!(cell("vram_reserved_kb"), "0");
}

/// The `--expect` key set is derived from `CSV_HEADER`, and the values are read
/// back out of `Serialize`. Those are two different views of the struct, so
/// this is the test that keeps them one: a field renamed on one side and not
/// the other would otherwise make every assertion on it silently unmeasurable.
#[test]
fn expectation_keys_match_the_metrics() {
    let m = sample_metrics();
    for key in metric_keys() {
        // `ground_m` is the deliberate exception: it is `NaN` here, which is
        // JSON null, which is "not measured" — and that is the answer.
        if key == "ground_m" {
            assert!(m.measure(key).is_none(), "NaN should read as unmeasured");
            continue;
        }
        assert!(
            m.measure(key).is_some(),
            "{key} is an --expect key but has no value"
        );
    }
    // The two non-numeric fields are excluded on purpose, not by accident.
    assert!(!metric_keys().any(|k| k == "name" || k == "busy"));
}

#[test]
fn expectations_parse_every_operator_and_booleans() {
    let cases = [
        ("globe_rendered>0", 0.0),
        ("globe_rendered >= 1", 1.0),
        ("space_factor<0.5", 0.5),
        ("alt_m<=180000", 180000.0),
        ("settled==true", 1.0),
        ("software_renderer!=false", 0.0),
    ];
    for (spec, want) in cases {
        let e = Expectation::parse(spec).unwrap_or_else(|err| panic!("{spec:?}: {err}"));
        assert_eq!(e.want, want, "{spec:?}");
        // The log line shows what was typed, not the parsed form.
        assert_eq!(e.to_string(), spec);
    }
}

/// A typo'd assertion that quietly holds is worse than no assertion — a green
/// job then claims something nobody checked. Every malformed spec must die at
/// parse time, before the app is even built.
#[test]
fn malformed_expectations_are_rejected_at_parse_time() {
    for bad in [
        "globe_renderd>0",
        "settled=true",
        "globe_rendered",
        "alt_m>banana",
        "alt_m>NaN",
    ] {
        assert!(Expectation::parse(bad).is_err(), "{bad:?} should not parse");
    }
    // And the error names the metrics, so a typo is one message from a fix.
    let err = Expectation::parse("globe_renderd>0").unwrap_err();
    assert!(err.contains("globe_rendered"), "{err}");
}

#[test]
fn expectations_hold_or_report_the_measured_value() {
    let mut m = sample_metrics();
    m.globe_rendered = 42;

    assert!(Expectation::parse("globe_rendered>0")
        .unwrap()
        .check(&m)
        .is_ok());
    assert!(Expectation::parse("settled==true")
        .unwrap()
        .check(&m)
        .is_err());

    // A failure carries the number, so a red run needs no second run to say why.
    let why = Expectation::parse("globe_rendered>100")
        .unwrap()
        .check(&m)
        .unwrap_err();
    assert!(why.contains("42"), "{why}");

    // An unmeasured metric is its own message, not a comparison against nothing.
    let why = Expectation::parse("ground_m>0")
        .unwrap()
        .check(&m)
        .unwrap_err();
    assert!(why.contains("not measured"), "{why}");
}

#[test]
fn a_bad_expectation_fails_the_whole_run_setup() {
    let result = ShotHarness::from_cli(ShotArgs {
        path: "/tmp/s.png".into(),
        at: &["0,0,1000".into()],
        expect: &["globe_rendered>0".into(), "nonsense>1".into()],
        ..default()
    });
    match result {
        Err(e) => assert!(e.starts_with("--expect"), "got {e:?}"),
        Ok(_) => panic!("an unknown metric must not build a harness"),
    }
}
