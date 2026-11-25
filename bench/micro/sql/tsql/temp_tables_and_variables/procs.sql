CREATE TABLE dbo.Items (
    ItemID INT NOT NULL
)
GO
CREATE PROCEDURE dbo.CopyItems
AS
BEGIN
    DECLARE @ids TABLE (ItemID INT);
    INSERT INTO @ids SELECT ItemID FROM dbo.Items;
    SELECT ItemID INTO #scratch FROM @ids;
    SELECT * FROM #scratch;
END
GO
