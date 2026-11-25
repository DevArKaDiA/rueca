namespace App;

public class Math2
{
    public int Fact(int n)
    {
        return n <= 1 ? 1 : n * Fact(n - 1);
    }
}
