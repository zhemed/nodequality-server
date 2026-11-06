# nodequality-server

NodeQuality 的报告托管后端：**收包、存包、发包**，不做渲染。

报告页的排版和终端渲染全部在前端完成（见 [nodequality-web](../nodequality-web)），
所以这个服务刻意保持很薄，一个二进制、一个目录就能跑。

## 接口

### 上传

```
POST /api/v1/record
Content-Type: text/plain

<base64 编码的 zip>
```

响应是**纯文本链接**，与上游客户端的行为兼容：

```
bash <(curl -sL <脚本地址>)
→ 输出：http://127.0.0.1:8080/r/<token>
```

### 读取

```
GET /api/v1/record/{token}
```

```json
{
  "success": true,
  "data": {
    "token": "……",
    "asn": null,
    "asOrganization": null,
    "location": null,
    "result": "<base64 的 zip>",
    "provider": null
  }
}
```

字段形状与上游保持一致，方便前端复用同一套解析逻辑。

## 运行

```bash
cargo run --release
```

| 环境变量 | 默认值 | 说明 |
| --- | --- | --- |
| `NQ_ADDR` | `0.0.0.0:8080` | 监听地址 |
| `NQ_DATA_DIR` | `./data` | 报告存储目录 |
| `NQ_WEB_DIR` | `./web` | 前端构建产物目录 |
| `NQ_BASE_URL` | `http://<监听地址>` | 生成外链时用的前缀 |

## 存储

每条报告一个目录：

```
data/<token>/
├── report.zip   # 客户端上传的原始 zip，字节级保存
└── meta.json    # 创建时间、大小、商家
```

## 与上游的关系

上游只开源了测试脚本，网站与后端一直闭源。这里是照着其公开接口契约的独立实现，
详见 [NOTICE](./NOTICE)。
