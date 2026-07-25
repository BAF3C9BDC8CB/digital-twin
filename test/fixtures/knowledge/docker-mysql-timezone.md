# Docker MySQL 时区踩坑

# @knowledge domain="运维" concept="docker-mysql-timezone" definition="Docker MySQL 时区设置，启动时必须加 TZ=Asia/Shanghai 环境变量，否则比北京时间慢 8 小时"
# @knowledge domain="运维" concept="docker-mysql-timezone" pitfall="Docker MySQL 容器默认时区是 UTC，不设置 TZ 会导致数据库时间差 8 小时，影响业务日志时间"
# @knowledge domain="运维" concept="docker-mysql-timezone" experience="启动 MySQL 容器必须加 -e TZ=Asia/Shanghai 环境变量"

## 问题

Docker 启动 MySQL 后，查询 `SELECT NOW()` 返回的是 UTC 时间，比北京时间慢 8 小时。

## 原因

MySQL 官方 Docker 镜像默认时区是 UTC，未继承宿主机时区。

## 解决方案

```bash
docker run -d --name mysql -e TZ=Asia/Shanghai -e MYSQL_ROOT_PASSWORD=root -p 3306:3306 mysql:8.0
```

或者在容器内执行：`SET GLOBAL time_zone = '+8:00';`
