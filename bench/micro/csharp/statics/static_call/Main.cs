namespace App;

public static class MathUtil
{
    public static int Square(int x)
    {
        return x * x;
    }
}

public class Area
{
    public int Of(int side)
    {
        return MathUtil.Square(side);
    }
}
