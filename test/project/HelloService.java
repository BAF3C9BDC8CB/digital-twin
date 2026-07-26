package com.example.order;

/**
 * Order service for processing customer orders.
 */
public class HelloService {

    /**
     * Create a new order and persist to database.
     */
    public Order createOrder(OrderRequest request) {
        saveToDb(request);
        sendNotification(request);
        return new Order();
    }

    private void saveToDb(OrderRequest r) {
        System.out.println("saving");
    }

    private void sendNotification(OrderRequest r) {
        EmailService.send(r);
    }
}
