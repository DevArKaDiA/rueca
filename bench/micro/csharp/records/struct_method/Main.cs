namespace App;

public struct Vector
{
    public double X;

    public double Length()
    {
        return X;
    }
}

public class Physics
{
    public double Speed(Vector v)
    {
        return v.Length();
    }
}
