package com.example.order;

import org.springframework.web.bind.annotation.*;
import org.springframework.beans.factory.annotation.Autowired;

/**
 * Order management controller.
 * Creates orders and calls pay-service for payment processing.
 */
@RestController
@RequestMapping("/api/orders")
public class OrderController {

    @Autowired
    private OrderService orderService;

    /**
     * 创建订单并调用支付服务完成支付
     */
    @PostMapping("/create")
    public Result createOrder(@RequestBody OrderRequest request) {
        Order order = orderService.createOrder(request);
        // Calls pay-service to process payment
        return Result.ok(order);
    }
}
// Minimal stubs so the file compiles conceptually
class OrderService { Order createOrder(Object r) { return new Order(); } }
class Order {}
class OrderRequest {}
class Result { static Result ok(Object o) { return null; } }
