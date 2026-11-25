namespace App;

public class Counter
{
    private int _n;

    public void Inc()
    {
        _n++;
    }

    public void IncTwice()
    {
        this.Inc();
        this.Inc();
    }
}
