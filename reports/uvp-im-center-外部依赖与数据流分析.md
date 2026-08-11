# uvp-im-center 外部依赖与数据流分析报告

> 分析对象：`/data/aflmProjects/aflm/uvp-im-center`（Spring Boot 微服务，端口 8869，应用名 `uvp-im-center`）
> 分析方法：源码直读（pom/配置/核心类）+ Nacos 配置导出（`/data/aflmProjects/aflm/nacos_config/`）+ digital-twin KG 检索佐证（`dt search_kg`，命中 `dt://entity/im-center/Service/腾讯im`、`im-center/Service/nacos config`、`im-center/Config/connecttimeout` 等实体，与源码结论一致）

## 0. 项目本质（先于清单的结论）

**uvp-im-center 不是自建 IM 服务，而是腾讯云即时通信 IM（TIM）REST API 的 HTTP 封装/代理网关**（readme 仅 8 字节："腾讯im"）。所有 IM 能力（单聊/群聊/账号/资料/关系链/最近联系人/运营）均通过 OkHttp 直接 POST 调用腾讯开放接口：

```
https://console.tim.qq.com/v4/{service}/{command}?sdkappid={sdkAppId}&identifier=administrator&usersig={UserSig}&random={rand}&contenttype=json
```

**关键否定结论：无 FeignClient、无消息队列（RabbitMQ/Kafka/RocketMQ）、无 Redis 使用、无 WebSocket、无内部下游服务调用。**

## 1. 外部依赖清单

| 依赖类型 | 名称/地址 | 用途 | 状态 |
|---|---|---|---|
| 父 POM | `com.nextai.uvp:nextai-uvp-platform:2.1.0-RELEASE` | 平台统一版本管理 | 内部仓库 |
| 内部 starter | `common-spring-boot-starter:2.2.0`（已排除 redis-starter、db-starter、fastjson） | 公共实体/工具 | 使用 |
| 内部 starter | `log-spring-boot-starter:2.1.0` | 日志中心 | 使用 |
| 注册/配置中心 | Nacos：test=`nacos.newoffen.net`（ns `af6d04ec-...`），prod=`nacos.newoffen.com`（discovery ns `0e5dee28-...`，config ns `5b5df484-...`），dev=`devnacos.taoyiyun.cn:80`；共享配置 DataId `common.yaml` | 服务注册发现 + 动态配置（`@RefreshScope`） | 使用 |
| 腾讯云 IM | `console.tim.qq.com/v4/*`，3 个租户应用：`1400724435`(yyt)、`1400599792`(test)、`1400687667`(yxhlwyy)，管理员账号 `administrator`，本地 HMAC-SHA256 生成 UserSig（自动续期） | 全部 IM 能力（openim / group_open_http_svc / profile / sns / recentcontact / operation） | **核心外部依赖** |
| MongoDB | 三节点副本集 `prod-mongo1/2/3.newoffen.com:27017`，库 `im`，用户 `root`，读偏好 `secondaryPreferred` | 消息记录、群记录、IM 账号映射持久化 | 使用 |
| HTTP 客户端 | OkHttp 4.9.2（调腾讯 IM）；Apache HttpClient 4.5.x + RestTemplate（`httpClientTemplate` 连接池，实际业务未用） | HTTP 调用 | OkHttp 使用 |
| Sentinel | `spring-cloud-starter-alibaba-sentinel`，dashboard `127.0.0.1:8080`（无效地址） | 限流熔断 | 依赖存在，规则未配置 |
| MySQL/Druid/MyBatis-Plus 3.0.7/pagehelper | `mysql-connector-java`、`druid-spring-boot-starter 1.1.10`、`mybatis-plus-boot-starter`、`mybatis-plus-generator` | 代码中 **0 处** `@Mapper/BaseMapper/@TableName` 使用 → 冗余依赖 | 未使用 |
| tencentcloud-sdk-java 3.1.519 | 腾讯云官方 SDK | 仅出现在注释（TRTC 示例） | 未使用 |
| jjwt 0.9.1 / fastjson 1.2.76 / jackson / guava / hutool | 工具库 | SigUtil、JSON 序列化 | 使用 |
| spring-boot-starter-web / aop / data-mongodb | — | Web 层、AOP、Mongo 模板 | 使用 |
| 运维设施 | K8s charts（harbor.offenhealth.com/project/uvp-im-center）、Spring Boot Admin（`http://boot.newoffen.com`）、Skywalking（关闭） | 部署/监控 | — |

**无**：openfeign、spring-boot-starter-amqp/rabbitmq、kafka、websocket、redis-spring-boot-starter（被显式排除）。
charts/values.yaml 中 `kafka01/02/03` 主机别名、`common.properties` 中的 redis/es/kafka/OSS 配置均为平台公共模板，im-center 不加载（bootstrap 仅引用 `common.yaml`）。

## 2. 消息数据流（来源 → 存储 → 分发）

### 2.1 单聊消息（核心链路）
```
上游业务服务 ──HTTP POST /message/sendMessage──▶ MessageService.sendMessage
      │  (组装 SendMsgRequest, syncOtherMachine=1)
      ▼
ImClient(OkHttp) ──▶ 腾讯云 IM openim/sendmsg ──▶ 返回 msgKey/msgTime
      │ (ActionStatus=OK)
      ▼
saveMessageRecord(msgKey,msgTime,from,to)  [mypool 线程池异步]
      │  回查腾讯 openim/admin_getroammsg（minTime=t-1s, maxTime=t+1s, maxCnt=3, 按 msgKey 匹配）
      ▼
MongoDB im.message_record_{source}   (_id = msgKey)
```

- **查询**：`POST /message/getRoamMsg`。`isLocal=1` 时走 MongoDB 聚合（按 From/To_Account 双向、`MsgFlagBits=0` 过滤撤回、MsgTimeStamp 倒序分页）；否则透传腾讯 `admin_getroammsg`。
- **撤回**：`/message/msgWithdraw` → 腾讯 `admin_msgwithdraw` 成功后异步把本地记录 `MsgFlagBits` 置 8（`sleep(1000)` 后 updateFirst）；若改 0 条则写 `message_record_error_{source}` 待人工处理。

### 2.2 群聊消息
```
POST /group/sendGroupMsg ──▶ 腾讯 group_open_http_svc/send_group_msg ──▶ MsgSeq/MsgTime
      ▼ (异步, sleep(3000) 后回查 group_msg_get_simple, 按 MsgSeq 匹配)
MongoDB im.group_record_{source}   (_id=groupId, RspMsgList 数组 addToSet 追加)
```
- 建群时**预建** `group_record_{source}` 空文档（避免并发写），`/group/createGroup`。
- 查询：`/group/groupMsgGetSimple` 支持 `isLocal=1` 走 Mongo unwind 聚合；撤回/删成员消息置 `RspMsgList.$.IsPlaceMsg` 标记。
- 其余群组接口（成员、资料、禁言、通知等）全部为腾讯 REST 透传。

### 2.3 账号数据流
```
POST /account/accountImport ──▶ Mongo 幂等检查(_id=source_originalId, status)
      ──▶ 腾讯 account_import ──▶ 异步存 Mongo im_account{_id, nick, faceUrl, status, source, domainId}
POST /account/accountImportAuto ──▶ 按 source 分页读 Mongo 批量导入腾讯
POST /account/queryOnlineStatus ──▶ 腾讯 状态查询透传
```

### 2.4 多租户（domain 路由）
`SourceInterceptor` 从请求头 `domainId` 匹配 Nacos 配置 `param.configs[]` → 选 sdkAppId/密钥/source；ThreadLocal（`SourceHolder`/`ImConfigContext`）+ TTL 线程池传递到异步任务。**无 domainId 时静默使用第一个租户**。所有 Mongo 集合按 `_{source}` 后缀物理分库隔离（message_record_/group_record_/message_record_error_ 各 ×N 租户）。

### 2.5 回调（腾讯 → im-center）
- 仅 `/trtc/{domain}/callback`、`/room/callback`、`/mate/callback` 三个端点，收到后**只打印**。
- `model/callback/` 下 20+ 个 TIM 回调模型（BeforeSendMsg、AfterSendMsg、StateChange、好友/黑名单/群回调等）**没有任何 Controller/Listener 消费 → 死代码**。
- 含义：腾讯 IM 的"消息回调"未接入，服务端拿消息只能靠"发完主动回查"的轮询式写库，而非回调推送。

## 3. 消息队列拓扑

**不存在。** 无交换机/队列/路由 key/消费者。全工程唯一命中"queue"的是 `ExecutorConfig.setQueueCapacity(5)`（线程池队列，非 MQ）。异步落库完全依赖自建线程池 `mypool`（core=20/max=30/queue=5，CallerRunsPolicy）。

## 4. 发现的架构问题

1. **消息记录可靠性差（无 MQ 的代价）**：写库靠"发送后按 `msgTime±1s` 窗口回查腾讯"——窗口竞态 + 群消息硬编码 `sleep(3s)` 碰运气；失败仅 log，**无重试、无补偿、无死信**（撤回同步失败才有一条 error 集合兜底）。
2. **回调体系悬空**：回调模型齐全却不消费，丢掉了状态变更/消息事件等增量数据；只能主动轮询腾讯，成本高且不一致。
3. **线程池队列过浅**：queue=5、CallerRunsPolicy，突发流量直接回压调用线程（阻塞上游 HTTP）；异步任务异常静默（`executor.execute` 无 try 包裹的代码多处）。
4. **冗余依赖**：MySQL/Druid/MyBatis-Plus/pagehelper 全量声明但零使用；tencentcloud-sdk-java 仅注释引用；Sentinel dashboard 指向 `127.0.0.1:8080` 无效地址。
5. **敏感信息明文**：Nacos 配置导出中含 Mongo 口令（root/mysql123）、腾讯 IM 私钥 `key`、OSS AK/SK 明文；`ImClient.getUrl()` 每次请求把**含 usersig 的完整 URL 打进日志**。
6. **越权/鉴权风险**：无 domainId 请求静默落到第一个租户；`/trtc/**` 被拦截器排除且无鉴权。
7. **主从读延迟**：Mongo 全局 `secondaryPreferred`，`isLocal=1` 历史查询可能读到从库延迟数据。
8. **生产配置缺陷**：Nacos `uvp-im-center.yaml` 中 `logging.level.org.springframework.data.mongodb.core.MongoTemplate: DEBUG` → 生产环境打全量 Mongo 操作日志。
9. **硬编码魔法数**：撤回/群记录 `sleep(1000/3000)`、`msgTime±1s` 拉取窗口、`expireTime-100` 提前续期，均无配置化。
10. **无内部服务调用**：不依赖 user-center/oauth-center（KG 检索无 im-center 的 Feign 实体佐证），鉴权、用户数据一致性完全依赖调用方网关，im-center 自身无任何安全校验。

## 5. dt search_kg 佐证记录

- `dt://entity/im-center/Service/腾讯im`（source: readme.md）—— 佐证"封装腾讯 IM"。
- `dt://entity/im-center/Service/nacos config`，关系 `uses common.yaml`（evidence: `shared-dataids: common.yaml`）、`has nacos命名空间 5b5df484-...`、`configures nacos服务器地址 nacos.newoffen.com` —— 与 bootstrap.yml 逐项一致。
- `dt://entity/im-center/Config/connecttimeout`（source: application.yml http-client.pool）—— 佐证 HTTP 连接池配置。
- 检索"im-center Feign 下游服务"无任何 im-center Feign 实体 —— 佐证无 Feign/无下游服务调用。
- KG 中 message-center（友盟推送）相关实体与本服务无关，注意区分：**im-center ≠ message-center**（前者腾讯 IM 通道，后者推送中心）。
