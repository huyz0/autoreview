public class Account {
    private int balance;

    public int getBalance() {
        return balance;
    }
    public void deposit(int amount) {
        balance += amount;
        notifyListeners();
        log("deposit");
    }
    public void withdraw(int amount) {
        balance -= amount;
    }
}
