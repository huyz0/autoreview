class Order {
    @ManyToOne(fetch = FetchType.LAZY)
    Customer customer;
}
