import org.mockito.Mockito;
import static org.mockito.Mockito.verify;
class Sample {
    void check(Runnable worker) {
        Mockito.verify(worker).run();
        org.mockito.Mockito.verify(worker, Mockito.never()).run();
        CustomVerifier.verify(worker);
        verify(worker).run();
    }
}
