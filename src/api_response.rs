// Copyright (c) 2024 구FS, all rights reserved. Subject to the MIT licence in `licence.md`.
use std::str::FromStr;


/// # Summary
/// Gallery detail response from "nhentai.net/api/v2/galleries/{id}".
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct GalleryDetailResponse
{
    pub id: u32,
    pub media_id: String, // v2 returns media_id as string, database stores it as u32
    pub title: GalleryTitle,
    pub cover: CoverInfo,
    pub thumbnail: CoverInfo,
    #[serde(default)]
    pub scanlator: String,
    pub upload_date: i64, // unix timestamp [s]
    pub tags: Vec<TagResponse>,
    pub num_pages: u16,
    pub num_favorites: u32,
    #[serde(default)]
    pub pages: Vec<PageInfo>,
}

impl GalleryDetailResponse
{
    /// # Summary
    /// Parses the media_id into a u32. v2 returns it as a string, but the database stores it as u32. On failure warns and returns 0.
    ///
    /// # Returns
    /// - media_id as u32
    pub fn media_id_u32(&self) -> u32
    {
        return self.media_id.parse::<u32>().unwrap_or_else(|e|
        {
            log::warn!("Parsing media_id \"{}\" to u32 failed with: {e}. Using 0 instead.", self.media_id);
            0
        });
    }

    /// # Summary
    /// Collapses all page types into a single string, for example "jjpwg".
    ///
    /// # Returns
    /// - page types string
    pub fn page_types(&self) -> String
    {
        return self.pages.iter()
            .map(|page| image_type_from_path(&page.path).map_or_else(|_| String::new(), |t| format!("{t:?}"))) // short form per page, empty if extension unknown
            .collect::<Vec<String>>()
            .join("");
    }

    /// # Summary
    /// Converts the unix upload timestamp into a chrono DateTime.
    ///
    /// # Returns
    /// - upload date
    pub fn upload_date_utc(&self) -> chrono::DateTime<chrono::Utc>
    {
        return chrono::DateTime::<chrono::Utc>::from_timestamp(self.upload_date, 0).unwrap_or_default();
    }

    /// # Summary
    /// Write gallery to database. Either creates a new entry or updates an existing one with the same primary key.
    ///
    /// # Arguments
    /// - `db`: SQLite database
    ///
    /// # Returns
    /// - number of rows affected or sqlx::Error
    pub async fn write_to_db(&self, db: &sqlx::sqlite::SqlitePool) -> Result<u64, sqlx::Error>
    {
        const HENTAI_QUERY_STRING: &str = "INSERT OR REPLACE INTO Hentai (id, cover_type, media_id, num_favorites, num_pages, page_types, scanlator, title_english, title_japanese, title_pretty, upload_date) "; // query string for Hentai table
        const HENTAI_TAG_QUERY1_STRING: &str = "DELETE FROM Hentai_Tag WHERE hentai_id IN "; // cleanup query string, delete all Hentai_Tag entries with same hentai_id before in case hentai had some tags untagged
        const HENTAI_TAG_QUERY2_STRING: &str = "INSERT INTO Hentai_Tag (hentai_id, tag_id) "; // query string for Hentai_Tag table
        const TAG_QUERY_STRING: &str = "INSERT OR REPLACE INTO Tag (id, name, type, url) "; // query string for Tag table
        let mut db_tx: sqlx::Transaction<'_, sqlx::Sqlite>; // transaction for all queries
        let mut rows_affected: u64 = 0; // number of rows affected by query


        db_tx = db.begin_with("PRAGMA foreign_keys = OFF; BEGIN TRANSACTION;").await?; // start transaction, disable foreign key checks for performance

        let mut query: sqlx::query_builder::QueryBuilder<sqlx::Sqlite> = sqlx::query_builder::QueryBuilder::new(HENTAI_QUERY_STRING); // query for Hentai table
        query.push_values
        (
            std::iter::once(self),
            |mut builder, hentai|
            {
                builder
                    .push_bind(hentai.id)
                    .push_bind(image_type_from_path(&hentai.cover.path).map_or_else(|_| String::new(), |t| format!("{t:?}"))) // cover type from cover path extension
                    .push_bind(hentai.media_id_u32())
                    .push_bind(hentai.num_favorites)
                    .push_bind(hentai.num_pages)
                    .push_bind(hentai.page_types()) // collapse all page types into 1 string, otherwise have to create huge Hentai_Pages table or too many Hentai_{id}_Pages tables
                    .push_bind(if hentai.scanlator.is_empty() {None} else {Some(hentai.scanlator.clone())}) // convert "" to None, otherwise forward unchanged
                    .push_bind(if hentai.title.english.is_empty() {None} else {Some(hentai.title.english.clone())})
                    .push_bind(hentai.title.japanese.as_ref().and_then(|s| if s.is_empty() {None} else {Some(s.clone())}))
                    .push_bind(if hentai.title.pretty.is_empty() {None} else {Some(hentai.title.pretty.clone())})
                    .push_bind(hentai.upload_date_utc());
            }
        );
        rows_affected += query
            .build()
            .persistent(false) // don't cache query
            .execute(&mut *db_tx).await? // execute query
            .rows_affected(); // get number of rows affected

        if !self.tags.is_empty() // only run Tag query if there are any tags
        {
            let mut query: sqlx::query_builder::QueryBuilder<sqlx::Sqlite> = sqlx::query_builder::QueryBuilder::new(TAG_QUERY_STRING); // query for Tag table
            query.push_values
            (
                self.tags.iter(),
                |mut builder, tag|
                {
                    builder
                        .push_bind(tag.id)
                        .push_bind(&tag.name)
                        .push_bind(&tag.r#type)
                        .push_bind(&tag.url);
                }
            );
            rows_affected += query
                .build()
                .persistent(false) // don't cache query
                .execute(&mut *db_tx).await? // execute query
                .rows_affected(); // get number of rows affected
        }

        let mut query: sqlx::query_builder::QueryBuilder<sqlx::Sqlite> = sqlx::query_builder::QueryBuilder::new(HENTAI_TAG_QUERY1_STRING); // cleanup query for Hentai_Tag table
        query.push("(");
        query.push_bind(self.id);
        query.push(");\n");
        if !self.tags.is_empty() // only insert Hentai_Tag entries if there are any tags
        {
            query.push(HENTAI_TAG_QUERY2_STRING); // query for Hentai_Tag table
            query.push_values
            (
                self.tags.iter().map(|tag| (self.id, tag.id)), // contains tuples of (hentai_id, tag_id)
                |mut builder, (hentai_id, tag_id)|
                {
                    builder
                        .push_bind(hentai_id)
                        .push_bind(tag_id);
                }
            );
        }
        rows_affected += query
            .build()
            .persistent(false) // don't cache query
            .execute(&mut *db_tx).await? // execute query
            .rows_affected(); // get number of rows affected


        db_tx.commit().await?; // commit transaction
        return Ok(rows_affected);
    }
}


/// # Summary
/// Gallery title in multiple languages.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct GalleryTitle
{
    pub english: String,
    pub japanese: Option<String>,
    pub pretty: String,
}


/// # Summary
/// Cover or thumbnail image with relative path and dimensions.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct CoverInfo
{
    pub path: String,
    pub width: u32,
    pub height: u32,
}


/// # Summary
/// Full page image details.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PageInfo
{
    pub number: u16,
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub thumbnail: String,
    pub thumbnail_width: u32,
    pub thumbnail_height: u32,
}


#[derive(Clone, Eq, PartialEq)]
pub enum ImageType
{
    Gif,
    Jpg,
    Png,
    Webp,
}

impl<'de> serde::Deserialize<'de> for ImageType
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error> // str -> ImageType
    where
        D: serde::Deserializer<'de>,
    {
        let s_de: String = String::deserialize(deserializer)?;
        match Self::from_str(s_de.as_str())
        {
            Ok(o) => return Ok(o),
            _ => return Err(serde::de::Error::custom(format!("Invalid image type: \"{s_de}\""))),
        };
    }
}

impl serde::Serialize for ImageType
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error> // ImageType -> str
    where
        S: serde::Serializer,
    {
        let s: String = format!("{:?}", self);
        return serializer.serialize_str(s.as_str());
    }
}

impl std::fmt::Debug for ImageType
{
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result // ImageType -> str
    {
        return write!(f, "{}",
            match self
            {
                Self::Gif => "g", // only short form in program context (database)
                Self::Jpg => "j",
                Self::Png => "p",
                Self::Webp => "w",
            }
        );
    }
}

impl std::fmt::Display for ImageType
{
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result // ImageType -> str
    {
        return write!(f, "{}",
            match self
            {
                Self::Gif => "gif", // long form for output
                Self::Jpg => "jpg",
                Self::Png => "png",
                Self::Webp => "webp",
            }
        );
    }
}

impl std::str::FromStr for ImageType
{
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> // str -> ImageType
    {
        let image_type: ImageType = match s.to_lowercase().trim()
        {
            "g" | "gif" => Self::Gif,
            "j" | "jpg" | "jpeg" => Self::Jpg,
            "p" | "png" => Self::Png,
            "w" | "webp" => Self::Webp,
            _ => return Err(format!("Invalid image type: \"{s}\"")),
        };
        return Ok(image_type);
    }
}


#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize, sqlx::FromRow)]
pub struct Tag
{
    pub id: u32,
    pub name: String,
    pub r#type: String, // type is a reserved keyword, r#type resolves to type
    pub url: String,
}


/// # Summary
/// Tag as returned by the nhentai.net v2 API. Reduced to the database Tag table on write.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct TagResponse
{
    pub id: u32,
    pub r#type: String, // type is a reserved keyword, r#type resolves to type
    pub name: String,
    pub slug: String,
    pub url: String,
    pub count: u32,
    #[serde(default)]
    pub description: Option<String>,
}


/// # Summary
/// Lightweight gallery as returned in search results. Only the id is needed, the rest of the metadata is filled in lazily via the gallery detail endpoint.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct GalleryListItem
{
    pub id: u32,
}


/// # Summary
/// Paginated gallery search response from "nhentai.net/api/v2/search".
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SearchResponse
{
    pub result: Vec<GalleryListItem>,
    pub num_pages: u32,
}


/// # Summary
/// Short-lived archive download URL response from "nhentai.net/api/v2/galleries/{id}/download".
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct DownloadResponse
{
    pub url: String,
    pub expires_at: i64, // unix timestamp [s]
}


/// # Summary
/// Determines the image type from a file path by inspecting its extension.
///
/// # Arguments
/// - `path`: file path, for example "galleries/123/1.jpg"
///
/// # Returns
/// - image type or error
pub fn image_type_from_path(path: &str) -> Result<ImageType, String>
{
    let extension: &str = path.rsplit('.').next().unwrap_or_default(); // everything after the last dot
    return ImageType::from_str(extension);
}