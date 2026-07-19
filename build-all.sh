#!/bin/bash
# 构建所有已配置项目
# 用法: bash build-all.sh
# 先确保: cd /data/myProject/digital-twin-v2
set -e

LOG_FILE="/tmp/dt-build-all-$(date +%Y%m%d-%H%M%S).log"
exec > >(tee -a "$LOG_FILE") 2>&1

echo "=== 全项目构建开始 $(date) ==="
echo "日志文件: $LOG_FILE"
echo ""

# ══════════ 7个 base 组，共 66 个项目 ══════════

# ── 组1: /data/aflmProjects/aflm (25 projects) ──
echo ">>> 组1: aflm"
for pair in \
  "archive-api:/data/aflmProjects/aflm/archive-api" \
  "copartner-h5:/data/aflmProjects/aflm/copartner/copartner-h5" \
  "doctor-center:/data/aflmProjects/aflm/doctor-center" \
  "hospital-center:/data/aflmProjects/aflm/hospital-center" \
  "message-center:/data/aflmProjects/aflm/uv-message-center" \
  "api-gateway:/data/aflmProjects/aflm/uvp-api-gateway" \
  "app-center:/data/aflmProjects/aflm/uvp-app-center" \
  "comment-center:/data/aflmProjects/aflm/uvp-comment-center" \
  "im-center:/data/aflmProjects/aflm/uvp-im-center" \
  "knight-center:/data/aflmProjects/aflm/uvp-knight-center" \
  "label-center:/data/aflmProjects/aflm/uvp-label-center" \
  "med-alliance-center:/data/aflmProjects/aflm/uvp-med-alliance-center" \
  "medicals-center:/data/aflmProjects/aflm/uvp-medicals-center" \
  "nurse-center:/data/aflmProjects/aflm/uvp-nurse-center" \
  "oauth-center:/data/aflmProjects/aflm/uvp-oauth-center" \
  "order-center:/data/aflmProjects/aflm/uvp-order-center" \
  "user-center:/data/aflmProjects/aflm/uvp-user-center" \
  "boss-center:/data/aflmProjects/aflm/boss/uvp-boss-center" \
  "boss:/data/aflmProjects/aflm/boss/boss" \
  "copartner-center:/data/aflmProjects/aflm/copartner/uvp-copartner-center" \
  "copartner:/data/aflmProjects/aflm/copartner/copartner-h5" \
  "home-center:/data/aflmProjects/aflm/home/uvp-home-center" \
  "yijianbao-home:/data/aflmProjects/aflm/home/yijianbao-home" \
  "admin-center:/data/aflmProjects/aflm/admin/uvp-admin-center" \
  "admin:/data/aflmProjects/aflm/admin/uv-admin" \
; do
  name="${pair%%:*}"
  path="${pair#*:}"
  echo "  [$name] $path"
  dt build --path "$path" --name "$name" || echo "  [FAIL] $name"
done

# ── 组2: /data/aflmProjects/warehouse (9 projects) ──
echo ">>> 组2: warehouse"
for pair in \
  "caigou:/data/aflmProjects/warehouse/yyc-caigou" \
  "yaochang-gongsi:/data/aflmProjects/warehouse/yyc-yaochang-gongsi" \
  "goods-center:/data/aflmProjects/warehouse/goods/uvp-goods-center" \
  "goods-h5:/data/aflmProjects/warehouse/goods/goods-center-h5" \
  "business-center:/data/aflmProjects/warehouse/uvp-business-center" \
  "warehouse-center:/data/aflmProjects/warehouse/uvp-warehouse-center" \
  "warehouse-api:/data/aflmProjects/warehouse/uvp-warehouse-api" \
; do
  name="${pair%%:*}"
  path="${pair#*:}"
  echo "  [$name] $path"
  dt build --path "$path" --name "$name" || echo "  [FAIL] $name"
done

# ── 组3: /data/aflmProjects/others (12 projects) ──
echo ">>> 组3: others"
for pair in \
  "cashier:/data/aflmProjects/others/pay/offenpay-ui/offenpay-ui-cashier" \
  "manager:/data/aflmProjects/others/pay/offenpay-ui/offenpay-ui-manager" \
  "merchant:/data/aflmProjects/others/pay/offenpay-ui/offenpay-ui-merchant" \
  "offen-pay:/data/aflmProjects/others/pay/uvp-offen-pay" \
  "third-center:/data/aflmProjects/others/third-center" \
  "base-center:/data/aflmProjects/others/uvp-base-center" \
  "cache-center:/data/aflmProjects/others/uvp-cache-center" \
  "config-center:/data/aflmProjects/others/uvp-config-center" \
  "search-center:/data/aflmProjects/others/uvp-search-center" \
  "settlement-center:/data/aflmProjects/others/uvp-settlement-center" \
  "sms-center:/data/aflmProjects/others/uvp-sms-center" \
  "statistics-center:/data/aflmProjects/others/uvp-statistics-center" \
; do
  name="${pair%%:*}"
  path="${pair#*:}"
  echo "  [$name] $path"
  dt build --path "$path" --name "$name" || echo "  [FAIL] $name"
done

# ── 组4: /data/aflmProjects/unimportant (9 projects) ──
echo ">>> 组4: unimportant"
for pair in \
  "hospital-biz:/data/aflmProjects/unimportant/uv-net-hospital-biz" \
  "charge-center:/data/aflmProjects/unimportant/uvp-charge-center" \
  "content-center:/data/aflmProjects/unimportant/uvp-content-center" \
  "data-center:/data/aflmProjects/unimportant/uvp-data-center" \
  "log-center:/data/aflmProjects/unimportant/uvp-log-center" \
  "log-server:/data/aflmProjects/unimportant/uvp-log-server" \
  "pay-center:/data/aflmProjects/unimportant/uvp-pay-center" \
  "saas-warehouse:/data/aflmProjects/unimportant/uvp-saas-warehouse" \
  "yimeng-website:/data/aflmProjects/unimportant/yimeng-website" \
; do
  name="${pair%%:*}"
  path="${pair#*:}"
  echo "  [$name] $path"
  dt build --path "$path" --name "$name" || echo "  [FAIL] $name"
done

# ── 组5: /data/aflmProjects 根 (6 projects) ──
echo ">>> 组5: aflmProjects 根"
for pair in \
  "charts-prod:/data/aflmProjects/charts-prod" \
  "charts-test:/data/aflmProjects/charts-test" \
  "inner-intergration:/data/aflmProjects/uvp-inner-intergration" \
  "yijianbao:/data/aflmProjects/yijianbao" \
  "yiyuantong:/data/aflmProjects/yiyuantong" \
  "shopyijianbao:/data/aflmProjects/shopyijianbao_shop" \
; do
  name="${pair%%:*}"
  path="${pair#*:}"
  echo "  [$name] $path"
  dt build --path "$path" --name "$name" || echo "  [FAIL] $name"
done

# ── 组6: /data/aflmProjects/yijianbao (1 project) ──
echo ">>> 组6: yijianbao"
dt build --path "/data/aflmProjects/yijianbao/yingchao_web" --name "yingchao-web" \
  || echo "  [FAIL] yingchao-web"

# ── 组7: /data/myProject (5 projects, 不含 digital-twin-v2 已构建) ──
echo ">>> 组7: myProject"
for pair in \
  "digital-twin:/data/myProject/digital-twin" \
  "svc:/data/myProject/svc" \
  "kub:/data/myProject/kub" \
  "jcli:/data/myProject/jenkins-cli-rs" \
  "neatReader:/data/myProject/neatReader" \
; do
  name="${pair%%:*}"
  path="${pair#*:}"
  echo "  [$name] $path"
  dt build --path "$path" --name "$name" || echo "  [FAIL] $name"
done

echo ""
echo "=== 全部完成 $(date) ==="
echo "日志: $LOG_FILE"
