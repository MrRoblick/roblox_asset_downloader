use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Creator{
    pub id: u64,
    pub name: String,
    pub creator_type: String,
    pub creator_target_id: u64,
    pub has_verified_badge: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct AssetDetailsResponse{
    pub target_id: u64,
    pub asset_id: u64,
    pub name: String,
    pub description: String,
    pub asset_type_id: u8,
    pub creator: Creator,
    pub icon_image_asset_id: u64,
    pub created: String,
    pub updated: String,
}


#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupGamesResponse {
    pub next_page_cursor: Option<String>,
    pub data: Vec<GameDetail>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootPlace {
    pub id: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameDetail {
    pub id: u64,
    pub name: String,
    pub root_place: RootPlace,
}



#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetBatchRequest {
    pub asset_id: u64,
    pub request_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetBatchResponseItem {
    pub locations: Option<Vec<AssetLocation>>,
    pub request_id: String,
    pub is_archived: bool,
    pub asset_type_id: u32,
    pub is_recordable: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetLocation {
    pub asset_format: String,
    pub location: String,
    pub asset_metadatas: Option<Vec<AssetMetadata>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetMetadata {
    pub metadata_type: u32,
    pub value: String,
}