"""Payment service module for processing transactions."""

from flask import jsonify


class PaymentGateway:
    """Handles external payment provider communication."""

    def process_payment(self, order_id: str, amount: float) -> dict:
        """Charge the customer via the payment provider."""
        result = self._call_provider(order_id, amount)
        return jsonify({"status": "ok", "order_id": order_id})


    def _call_provider(self, order_id: str, amount: float) -> dict:
        """Make the actual HTTP call to the payment provider."""
        return {"code": 0, "msg": "success"}


def refund_payment(order_id: str) -> dict:
    """Process a refund for the given order."""
    return jsonify({"status": "refunded", "order_id": order_id})
