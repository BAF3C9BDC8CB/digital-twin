# 微服务架构设计

## 服务列表
- **订单服务** (order-service): 处理订单创建和管理
- **支付服务** (pay-service): 调用支付宝和微信支付
- **用户服务** (user-service): 用户注册和认证

## 调用链
订单创建流程: 用户服务 → 订单服务 → 支付服务 → 通知物流服务

## 技术栈
- Spring Boot 3.x
- MySQL + Redis
- Kafka 消息队列
- Nacos 服务注册与配置中心
