use rusqlite::{Connection, Result};

pub fn init_database(db_path: &str) -> Result<Connection> {
    let conn = Connection::open(db_path)?;

    // Create regions table
    conn.execute(
        "CREATE TABLE IF NOT EXISTS regions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            latitude REAL NOT NULL,
            longitude REAL NOT NULL,
            tile_x INTEGER NOT NULL,
            tile_y INTEGER NOT NULL,
            tile_z INTEGER NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        [],
    )?;

    // Create index on tile coordinates
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_regions_tile ON regions(tile_x, tile_y, tile_z)",
        [],
    )?;

    // Seed default region if database is empty
    seed_default_region(&conn)?;

    Ok(conn)
}

/// Seed a default region (Groningen) if no regions exist
fn seed_default_region(conn: &Connection) -> Result<()> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM regions", [], |row| row.get(0))?;

    if count == 0 {
        use vibe_core::world::{lat_lng_to_tile, REGION_ZOOM_LEVEL};

        // Default region at Groningen, Netherlands
        let groningen_lat = 53.2194;
        let groningen_lng = 6.5665;
        let (tile_x, tile_y) = lat_lng_to_tile(groningen_lat, groningen_lng, REGION_ZOOM_LEVEL);

        conn.execute(
            "INSERT INTO regions (name, latitude, longitude, tile_x, tile_y, tile_z, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'), datetime('now'))",
            rusqlite::params![
                "Groningen",
                groningen_lat,
                groningen_lng,
                tile_x,
                tile_y,
                REGION_ZOOM_LEVEL as i64,
            ],
        )?;

        println!("✅ Seeded default region: Groningen");
    }

    Ok(())
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // created_at/updated_at kept for future audit/display
pub struct RegionRow {
    pub id: i64,
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub tile_x: i64,
    pub tile_y: i64,
    pub tile_z: i64,
    pub created_at: String,
    pub updated_at: String,
}

