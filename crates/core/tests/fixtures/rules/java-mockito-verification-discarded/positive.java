import org.mockito.Mockito;
class Sample {
    void check(Runnable worker) {
        Mockito.verify(worker);
    }
}
