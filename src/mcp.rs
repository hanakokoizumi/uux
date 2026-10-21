//! MCP protocol adapter. UU account and connection state live in the shared service.
use anyhow::Result;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt, model::*, service::RequestContext};
use serde_json::{Value, json};
use std::{path::Path, sync::Arc};
#[derive(Clone)]
struct Server {
    client: Arc<crate::ipc::Client>,
}
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
                    .with_title("UUX"),
            )
            .with_instructions("UUX Linux-native remote controller. Obtain device IDs with uu_devices_list, then open a session. Use task IDs to observe long operations. local_path is on the Linux server; remote paths are on the controlled device. Desktop coordinates reference a screenshot frame_id. Commands return merged terminal output. Closing your session does not close another client's use of it.")
    }
    fn get_tool(&self, name: &str) -> Option<Tool> {
        crate::mcp_api::catalog()
            .into_iter()
            .find(|t| t.name == name)
    }
    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: crate::mcp_api::catalog(),
            ..Default::default()
        })
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let client = self.client.clone();
        let name = request.name.to_string();
        let arguments = request.arguments.map(Value::Object).unwrap_or(json!({}));
        let result = tokio::task::spawn_blocking(move || {
            client.call(json!({"op":"call","name":name,"arguments":arguments}))
        })
        .await;
        let result = match result {
            Ok(Ok(mut value)) => {
                let image = value
                    .as_object_mut()
                    .and_then(|o| o.remove("image_base64"))
                    .and_then(|v| v.as_str().map(str::to_owned));
                let mut result = CallToolResult::structured(value.clone());
                if let Some(image) = image {
                    result.content.push(ContentBlock::image(
                        image,
                        value["mime_type"].as_str().unwrap_or("image/png"),
                    ));
                }
                result
            }
            Ok(Err(e)) => CallToolResult::structured_error(json!({"error":e.to_string()})),
            Err(e) => {
                CallToolResult::structured_error(json!({"error":format!("任务执行失败：{e}")}))
            }
        };
        Ok(result.into())
    }
}
pub fn run(state: Option<&Path>) -> Result<()> {
    let client = Arc::new(crate::ipc::Client::connect(state, "mcp")?);
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(async move {
            let release = client.clone();
            let service = Server { client }.serve(rmcp::transport::stdio()).await?;
            let result = service.waiting().await;
            release.detach();
            result?;
            Ok(())
        })
}
