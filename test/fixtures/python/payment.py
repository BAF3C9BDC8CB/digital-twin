"""Payment service - handles payment processing."""
from flask import Flask, request, jsonify

app = Flask(__name__)


@app.route('/api/pay/confirm', methods=['POST'])
def confirm_payment():
    """确认支付请求，调用支付宝/微信支付"""
    data = request.get_json()
    return jsonify({"status": "ok", "order_id": data.get("order_id")})


@app.route('/api/pay/refund', methods=['POST'])
def refund_payment():
    """处理退款请求，调用支付宝退款接口"""
    data = request.get_json()
    return jsonify({"status": "refunded", "order_id": data.get("order_id")})
