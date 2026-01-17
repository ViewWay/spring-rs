# spring-swagger 插件设计文档

**日期**: 2026-01-17
**版本**: 1.0.0
**作者**: Claude + Spring-rs Community

---

## 1. 概述

### 1.1 插件名称
`spring-swagger`

### 1.2 目的
为 spring-rs 提供自动化的 API 文档生成能力，基于 OpenAPI 3.1 规范，减少开发者手动编写文档的工作量。

### 1.3 核心价值
- **自动化**: 从代码和注解自动生成文档，保持文档与代码同步
- **标准化**: 遵循 OpenAPI 3.1 规范，便于与前端对接
- **易用性**: 内置 Swagger UI，可在浏览器直接测试 API
- **灵活性**: 支持多种配置方式和扩展点

---

## 2. 架构设计

### 2.1 目录结构
```
spring-swagger/
├── Cargo.toml
├── src/
│   ├── lib.rs              # 插件入口，Plugin trait 实现
│   ├── openapi.rs          # OpenAPI 规范生成核心
│   ├── schema.rs           # JSON Schema 生成（从 Rust 类型）
│   ├── collector.rs        # 路由信息收集器
│   ├── ui.rs               # Swagger UI / Redoc 静态资源服务
│   ├── security.rs         # 安全方案（JWT、OAuth2、API Key）
│   ├── config.rs           # 配置结构体
│   └── export.rs           # 文档导出（JSON/YAML/文件）
├── examples/
│   └── basic-example/      # 基础使用示例
└── swagger-ui/             # 内嵌的 Swagger UI 静态资源
```

### 2.2 核心依赖
| 依赖 | 版本 | 用途 |
|------|------|------|
| utoipa | ~5.4 | OpenAPI 规范生成 |
| utoipa-swagger-ui | ~5 | Swagger UI 集成 |
| utoipa-redoc | ~5 | Redoc UI（可选） |
| serde_json | 1 | JSON 序列化 |
| serde_norway | 0.9 | YAML 序列化 |

### 2.3 依赖关系
```
spring-swagger
    ├── spring (core)       # Plugin trait, 配置系统
    ├── spring-web          # 路由集成
    ├── spring-macros       # 宏扩展
    └── utoipa ecosystem    # OpenAPI 生成
```

---

## 3. 功能清单

| 功能分类 | 具体功能 | 优先级 |
|---------|---------|--------|
| **文档生成** | OpenAPI 3.1 规范 | P0 |
| | 自动 Schema 推导 | P0 |
| | 类型别名支持 | P1 |
| **UI 界面** | Swagger UI | P0 |
| | Redoc（可选） | P2 |
| | 自定义主题 | P2 |
| **安全支持** | JWT Bearer | P0 |
| | OAuth2 | P1 |
| | API Key | P1 |
| | HTTP Basic | P1 |
| **元数据** | Tags 分组 | P0 |
| | 多服务器配置 | P1 |
| | 外部文档链接 | P2 |
| **导出** | JSON | P0 |
| | YAML | P1 |
| | 构建时导出到文件 | P2 |
| **响应** | 多状态码 | P0 |
| | 响应示例 | P1 |
| | 可复用响应定义 | P1 |
| **请求** | 请求示例 | P1 |
| | 文件上传 | P1 |
| | Form 数据 | P1 |
| **高级** | API 嵌套合并 | P2 |
| | 运行时修改 | P2 |
| | 自定义扩展 | P2 |

---

## 4. 核心组件设计

### 4.1 配置结构 (config.toml)
```toml
[swagger]
enabled = true
ui_path = "/swagger"          # Swagger UI 访问路径
api_path = "/openapi.json"    # OpenAPI JSON 规范路径
yaml_path = "/openapi.yaml"   # OpenAPI YAML 规范路径（可选）
export_file = "openapi.json"  # 构建时导出文件路径（可选）

[swagger.info]
title = "My API"
version = "1.0.0"
description = "API Description"
terms_of_service = "https://example.com/terms/"
contact_name = "API Team"
contact_email = "api@example.com"
license_name = "MIT"
license_url = "https://opensource.org/licenses/MIT"

[[swagger.servers]]
url = "http://localhost:8080"
description = "Local development"

[[swagger.servers]]
url = "https://api.example.com"
description = "Production"
```

### 4.2 路由信息收集器 (collector.rs)

```rust
pub trait OpenApiCollector {
    fn collect_routes(&mut self, routes: &[Route]);
    fn collect_components(&mut self, schemas: Vec<Schema>);
    fn build_openapi(&self) -> OpenApi;
}

pub struct RouteInfo {
    pub path: String,
    pub method: HttpMethod,
    pub handler: String,
    pub params: Vec<Parameter>,
    pub request_body: Option<RequestBody>,
    pub responses: Vec<Response>,
    pub security: Vec<SecurityRequirement>,
    pub tags: Vec<String>,
    pub summary: Option<String>,
    pub description: Option<String>,
}
```

### 4.3 宏扩展 (spring-macros)

扩展现有路由宏，添加 OpenAPI 属性支持：

```rust
// 使用 utoipa 宏注解
#[utoipa::path(
    get,
    path = "/users/{id}",
    responses(
        (status = 200, description = "User found", body = User),
        (status = 404, description = "User not found")
    ),
    params(
        ("id" = u64, Path, description = "User ID")
    ),
    security(
        ("bearer_auth" = [])
    )
)]
#[get("/users/{id}")]
async fn get_user(Path(id): Path<u64>) -> Json<User> { ... }

// 可复用 Schema 定义
#[derive(ToSchema)]
struct User {
    id: u64,
    name: String,
    email: String,
}
```

### 4.4 安全方案 (security.rs)

```rust
pub enum SecurityScheme {
    Bearer(JwtConfig),
    ApiKey(ApiKeyConfig),
    OAuth2(OAuth2Config),
    Http(HttpConfig),
}

pub struct JwtConfig {
    pub bearer_format: String,
    pub scheme: String,
}

pub struct ApiKeyConfig {
    pub name: String,
    pub location: ApiKeyLocation, // Header, Query, Cookie
}
```

---

## 5. 数据流

```
┌─────────────┐
│  应用启动    │
└──────┬──────┘
       │
       ▼
┌─────────────────────────────────────┐
│  spring-swagger Plugin 初始化        │
│  - 加载配置                          │
│  - 注册 OpenAPI 服务路由              │
└──────┬──────────────────────────────┘
       │
       ▼
┌─────────────────────────────────────┐
│  Collector 扫描路由                   │
│  - 收集 #[route] 宏定义的路由          │
│  - 提取 utoipa 注解                   │
│  - 推导 Schema                       │
└──────┬──────────────────────────────┘
       │
       ▼
┌─────────────────────────────────────┐
│  构建 OpenAPI 规范                   │
│  - 合并所有路由信息                   │
│  - 生成完整 OpenAPI 3.1 文档          │
└──────┬──────────────────────────────┘
       │
       ▼
┌─────────────┬─────────────┬─────────────┐
│ /swagger    │/openapi.json│/openapi.yaml│
│ Swagger UI  │ API 规范    │ API 规范    │
└─────────────┴─────────────┴─────────────┘
```

---

## 6. API 设计

### 6.1 用户 API

```rust
use spring_swagger::{OpenApi, SwaggerUi};

#[auto_config]
struct SwaggerConfig {
    ui_path: String,           // default: "/swagger"
    api_path: String,          // default: "/openapi.json"
    yaml_path: Option<String>, // default: None
    enabled: bool,             // default: true
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "My API",
        version = "1.0.0",
        description = "API Description"
    ),
    paths(
        get_user,
        create_user,
        list_users
    ),
    components(schemas(User, CreateUser))
    tags(
        (name = "users", description = "User management")
    )
)]
struct ApiDoc;

fn main() {
    App::new()
        .add_plugin(SwaggerPlugin::new(ApiDoc::openapi()))
        .add_plugin(WebPlugin)
        .run();
}
```

### 6.2 宏 API

```rust
// 方式1: 基于 utoipa 宏
#[utoipa::path(...)]
#[get("/users/{id}")]
async fn get_user(...) { ... }

// 方式2: 基于 spring 宏扩展（未来）
#[get("/users/{id}")]
#[openapi(
    summary = "Get user by ID",
    description = "Retrieve a single user",
    response(200, User),
    response(404),
    security("bearer_auth")
)]
async fn get_user(...) { ... }

// 方式3: 基于结构体定义（推荐用于复杂请求）
#[derive(ToSchema, Deserialize)]
struct GetUserRequest {
    id: u64,
    include_profile: bool,
}
```

---

## 7. 使用示例

### 7.1 基础示例

```rust
use spring::prelude::*;
use spring_web::prelude::*;
use spring_swagger::OpenApi;
use utoipa::ToSchema;

#[derive(ToSchema)]
struct User {
    id: u64,
    name: String,
    email: String,
}

#[utoipa::path(
    get,
    path = "/users/{id}",
    responses(
        (status = 200, description = "User found", body = User),
        (status = 404, description = "User not found")
    ),
    params(
        ("id" = u64, Path, description = "User ID")
    )
)]
#[get("/users/{id}")]
async fn get_user(Path(id): Path<u64>) -> Result<Json<User>> {
    // ...
}

#[utoipa::path(
    post,
    path = "/users",
    request_body = User,
    responses(
        (status = 201, description = "User created", body = User)
    )
)]
#[post("/users")]
async fn create_user(Json(user): Json<User>) -> Result<Json<User>> {
    // ...
}

#[derive(OpenApi)]
#[openapi(
    info(title = "User API", version = "1.0.0"),
    paths(get_user, create_user),
    components(schemas(User))
)]
struct ApiDoc;

#[auto_config]
struct Config;

fn main() {
    App::with_config(Config)
        .add_plugin(WebPlugin)
        .add_plugin(SwaggerPlugin::new(ApiDoc::openapi()))
        .run();
}
```

### 7.2 带 JWT 认证的示例

```rust
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};

#[derive(OpenApi)]
#[openapi(
    info(title = "Secure API", version = "1.0.0"),
    paths(get_user),
    components(
        schemas(User),
        security_schemes(
            ("bearer_auth" = HttpBuilder::new()
                .scheme(HttpAuthScheme::Bearer)
                .bearer_format("JWT")
                .build())
        )
    )
)]
struct ApiDoc;

#[utoipa::path(
    get,
    path = "/users/{id}",
    responses(
        (status = 200, body = User)
    ),
    params(
        ("id" = u64, Path)
    ),
    security(
        ("bearer_auth" = [])
    )
)]
#[get("/users/{id}")]
async fn get_user(Path(id): Path<u64>) -> Result<Json<User>> {
    // ...
}
```

---

## 8. 实现计划

### Phase 1: 核心功能 (P0)
1. 创建 spring-swagger 插件基础结构
2. 实现 Plugin trait
3. 集成 utoipa 和 utoipa-swagger-ui
4. 实现基础配置加载
5. 实现 /openapi.json 端点
6. 实现 /swagger Swagger UI

### Phase 2: 宏集成 (P0-P1)
1. 扩展 spring-macros 支持 utoipa 宏
2. 实现路由信息收集器
3. 自动推导 Schema

### Phase 3: 高级功能 (P1-P2)
1. YAML 导出
2. 多种安全方案
3. 响应示例
4. Tags 分组
5. 多服务器配置

### Phase 4: 优化和工具 (P2)
1. 构建时文档导出
2. Redoc UI 选项
3. 运行时文档修改
4. 自定义主题

---

## 9. 测试计划

| 测试类型 | 覆盖范围 |
|---------|---------|
| 单元测试 | Schema 生成、配置解析 |
| 集成测试 | 端到端文档生成、UI 访问 |
| 示例测试 | examples/ 下所有示例可运行 |

---

## 10. 参考

- [utoipa 文档](https://docs.rs/utoipa/)
- [OpenAPI 3.1 规范](https://spec.openapis.org/oas/v3.1.0)
- [SpringFox (Java)](https://springdoc.org/)
- [Salvo OpenAPI](https://salvo.rs/guide/features/openapi/)
