# Tidy 遥测代理

一个 Cloudflare Worker，把桌面端的匿名统计转发给 PostHog，并在转发前抹掉客户端 IP。

## 为什么要这一跳

- **可达性**：`app.posthog.com` 在部分地区不稳。直连会让那部分用户的上报整段失败，日活看起来凭空少一截。
- **隐私**：应用发出的每条事件都带 `$geoip_disable`，但那只是请求服务端别查。真正的保证是 PostHog 拿不到 IP —— 这里就是掐掉 IP 的地方。

## 部署

```bash
cd tools/telemetry-proxy
npx wrangler deploy
```

**已部署地址**：`https://t.tandem-clip.com`（Worker 名 `tidy-telemetry`）

默认的 `*.workers.dev` 地址已在 `wrangler.toml` 里关闭，只留自定义域这一个入口。

域名用的是 `tandem-clip.com` 这个区，而不是产品域名 `tidy.talkwork.vip`——因为 Worker 绑自定义域要求该域名托管在 Cloudflare，而 `talkwork.vip` 在阿里云 DNS。这一点已在隐私页点名说明，免得有人抓包看到一个陌生域名。

## 接到桌面端

遥测地址和 PostHog 项目公钥都是**构建期**注入的。两个变量任意一个为空，遥测就整个编译不进二进制：

```bash
TIDY_TELEMETRY_URL=https://t.example.com \
TIDY_TELEMETRY_KEY=phc_xxxxxxxx \
make release
```

`TIDY_TELEMETRY_KEY` 是 PostHog 的 Project API Key（只写不读，天然公开，可以进构建产物）。

## 验证

```bash
curl -X POST https://t.tandem-clip.com/batch/ \
  -H 'content-type: application/json' \
  -d '{"api_key":"phc_xxx","batch":[{"event":"app_launched","properties":{"distinct_id":"smoke-test"}}]}'
# 期望：ok
```

`GET` 任意路径应当返回 404 —— 这个端点不对外提供任何可读内容。

**⚠️ 200 不代表 key 是对的。** PostHog 的摄取端点是异步的：API key 写错了它照样返回
200，然后把事件丢掉。上面这个 curl 只能证明"代理转发通了"，不能证明 key 正确。
唯一的验证方式是去 PostHog 的 Activity / Events 页面看事件有没有真的进来。
排查"没数据"时按这个顺序查：代理返回 200 → PostHog 是否收到 → key 是否属于该项目。

## 怎么确认事件真的到了

代理返回 200 只说明"转发通了"。要确认事件落进 PostHog，用 Personal API Key 查
（`.env` 里的 `POSTHOG_PERSONAL_KEY` / `POSTHOG_PROJECT_ID`）：

```bash
set -a; . ./.env; set +a
curl -s -X POST -H "Authorization: Bearer $POSTHOG_PERSONAL_KEY" \
  -H 'content-type: application/json' \
  "https://us.posthog.com/api/projects/$POSTHOG_PROJECT_ID/query/" \
  -d '{"query":{"kind":"HogQLQuery","query":"SELECT distinct_id, event, timestamp FROM events WHERE properties.$lib = '"'"'tidy'"'"' ORDER BY timestamp DESC LIMIT 20"}}'
```

`properties.$lib = 'tidy'` 是 Tidy 事件的筛选条件——这个 PostHog 项目可能同时
装着别的应用的数据，不加这个过滤会看到不属于 Tidy 的事件。

**两个排查时容易被误导的地方：**

1. **没有 `telemetry-queue.json` 不等于发送成功。** 那个文件只在发送**失败**时
   才写。发送还没触发（应用启动不到 60 秒）时同样没有文件。别拿它当成功信号。
2. **`kill -9` 仍会丢内存里的事件。** 退出冲刷挂在 Tauri 的 `RunEvent::Exit` 上，
   正常退出（Cmd+Q、关窗）会走到，并且会先落盘再发，所以不丢；但 SIGKILL 谁也
   救不了。测试时注意：用 `pkill` 杀进程**不会**触发退出路径，看起来像丢数据
   其实是没测到。
3. **PostHog 的查询有分钟级延迟。** 事件已经收下了，HogQL 里可能几分钟后才查得到，
   新建项目尤其慢。别在 30 秒内就下"没到"的结论。

## 采了什么

字段清单以 `site/{zh,en}/privacy` 为准，代码在 `desktop/crates/mole-telemetry/src/event.rs`。改动采集面时，这三处必须一起改。
