CREATE TABLE [dbo].[Orders] (
    [OrderID] INT NOT NULL PRIMARY KEY,
    [Total] MONEY NULL
)
GO

CREATE PROCEDURE [dbo].[GetOrders]
AS
BEGIN
    SET NOCOUNT ON;
    SELECT [OrderID], [Total] FROM [dbo].[Orders];
END
GO

CREATE PROCEDURE [dbo].[Report]
AS
BEGIN
    EXEC [dbo].[GetOrders];
END
GO
