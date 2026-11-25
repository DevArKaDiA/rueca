namespace App;

public class Calculator
{
    public int Add(int a, int b)
    {
        return a + b;
    }

    public int Twice(int a)
    {
        return Add(a, a);
    }
}
