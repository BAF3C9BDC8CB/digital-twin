# Nacos 配置热更新不生效踩坑

# @knowledge domain="配置" concept="nacos-hot-reload" definition="Nacos 配置变更后应用无法自动刷新，需要加 @RefreshScope 注解"
# @knowledge domain="配置" concept="nacos-hot-reload" pitfall="Nacos 改了配置但 @Value 字段不刷新，因为类没有加 @RefreshScope"
# @knowledge domain="配置" concept="nacos-hot-reload" experience="使用 Nacos 配置热更新时，@Value 所在的 Bean 必须标注 @RefreshScope"

## 问题

在 Nacos 上修改配置后，应用中的 `@Value("${xxx}")` 字段没有自动更新，必须重启服务。

## 原因

Spring Cloud Nacos 的配置刷新机制要求目标 Bean 标注 `@RefreshScope`。

## 解决方案

在配置类上加 `@RefreshScope`：
```java
@RefreshScope
@Component
public class AppConfig {
    @Value("${timeout:5000}")
    private int timeout;
}
```
