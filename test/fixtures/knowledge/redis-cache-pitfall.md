# Redis 缓存踩坑经验

# @knowledge domain="缓存" concept="redis-cache" definition="Redis 缓存层，用于加速热点数据读取，减少数据库压力"
# @knowledge domain="缓存" concept="redis-cache" pitfall="Redis 缓存击穿：热点 key 过期瞬间大量请求直接打到数据库，需要用互斥锁或永不过期策略"
# @knowledge domain="缓存" concept="redis-cache" experience="缓存预热很重要，上线前先把热点数据加载到 Redis，避免冷启动时数据库被压垮"
# @knowledge domain="缓存" concept="cachePenetration" definition="缓存穿透：查询不存在的数据，每次都绕过缓存直接查数据库"
# @knowledge domain="缓存" concept="cachePenetration" pitfall="缓存穿透需要用布隆过滤器或空值缓存来防护，否则恶意请求会压垮数据库"