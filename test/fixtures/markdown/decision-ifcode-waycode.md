# 架构决策：支付平台用 ifCode+wayCode 拆分

## 背景

支付系统需要支持多个支付渠道（支付宝、微信、银盛），每个渠道有不同的接口协议。
最初的设计是每个渠道一个独立的 Service 类，导致代码重复严重。

## 决策

采用 ifCode + wayCode 两级拆分：
- **ifCode**：支付渠道编码（ALIPAY、WECHAT、YINSHENG），决定路由到哪个支付平台
- **wayCode**：支付方式编码（APP、H5、SCAN），决定调用平台的哪个接口

## 理由

1. 渠道和方式是正交的，拆分后可以自由组合
2. 新增渠道只需加 ifCode，不需要改现有代码
3. 新增方式只需加 wayCode，不影响渠道逻辑

## 影响

- PayChannelService.createPay(ifCode, wayCode, channelExtra) 是核心入口
- channelExtra 字段包含渠道特殊参数，容易遗漏
- 银盛渠道的 channelExtra 必须传，否则签名失败