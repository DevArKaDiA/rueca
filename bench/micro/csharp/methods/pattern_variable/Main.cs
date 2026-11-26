namespace App;

public class Shape
{
}

public class Circle : Shape
{
    public double Radius()
    {
        return 1;
    }
}

public class Geometry
{
    public double Size(Shape s)
    {
        if (s is Circle c)
        {
            return c.Radius();
        }
        return 0;
    }
}
