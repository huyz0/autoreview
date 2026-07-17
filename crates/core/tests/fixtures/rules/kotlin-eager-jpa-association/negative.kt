class Order {
    @ManyToOne(fetch = FetchType.LAZY)
    lateinit var customer: Customer
}
