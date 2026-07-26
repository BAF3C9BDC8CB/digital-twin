package com.example.payment;

/**
 * Payment channel service — handles payment routing via ifCode.
 *
 * @knowledge domain="支付" concept="ifCode" definition="支付渠道编码，用于路由到不同支付平台（支付宝/微信/银盛）"
 * @knowledge domain="支付" concept="ifCode" pitfall="ifCode 必须与 wayCode 配合使用，单独传 ifCode 会导致路由失败"
 * @knowledge domain="支付" concept="channelExtra" definition="支付渠道扩展参数，JSON 格式，包含银盛需要的特殊字段"
 * @knowledge domain="支付" concept="channelExtra" pitfall="channelExtra 字段容易遗漏，银盛渠道不传 channelExtra 会导致签名失败"
 */
public class PayChannelService {

    private String ifCode;
    private String wayCode;
    private String channelExtra;

    /**
     * Create a payment order with the given channel code.
     * Routes to different payment platforms based on ifCode.
     */
    public String createPay(String ifCode, String wayCode, String channelExtra) {
        // Validate ifCode + wayCode combination
        if (ifCode == null || wayCode == null) {
            throw new IllegalArgumentException("ifCode and wayCode are required");
        }
        // Route to payment platform
        return doPay(ifCode, wayCode, channelExtra);
    }

    /**
     * Execute the actual payment call to the platform.
     */
    private String doPay(String ifCode, String wayCode, String channelExtra) {
        // Build payment request
        return "PAY-" + ifCode + "-" + wayCode;
    }

    /**
     * Refund a completed payment.
     */
    public String refundPay(String payId, String amount) {
        // Refund logic
        return "REFUND-" + payId;
    }
}