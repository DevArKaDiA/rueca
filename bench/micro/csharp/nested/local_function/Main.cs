namespace App;

public class Totals
{
    public int Sum(int[] values)
    {
        int Add(int a, int b) => a + b;
        var total = 0;
        foreach (var v in values)
        {
            total = Add(total, v);
        }
        return total;
    }
}
